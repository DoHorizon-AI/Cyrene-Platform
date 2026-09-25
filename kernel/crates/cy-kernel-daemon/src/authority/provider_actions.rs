//! Provider registration, inventory publication, and reconciliation actions.
//!
//! Provider facts remain resource observations; lifecycle authority stays in
//! the parent Kernel authority state.
//! Provider 注册、inventory 发布和协调操作。
//!
//! Provider 事实仍是资源观测；生命周期 authority 由父级 Kernel authority 状态持有。

use super::*;

impl KernelProviderAuthority for LocalKernelAuthority {
    fn register_provider(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        provider: semantic::Provider,
    ) -> Result<semantic::Provider, semantic::Rejection> {
        self.validate_context(context)?;
        if provider.identity.generation == 0 {
            return Err(Self::rejection(
                "GENERATION_INVALID",
                "provider session generation must be non-zero",
            ));
        }
        provider
            .validate()
            .map_err(|error| Self::rejection(error.reason_code, error.message))?;

        let key = (context.namespace.clone(), provider.identity.id.clone());
        let registered = {
            let mut providers = self
                .runtime
                .providers
                .lock()
                .expect("provider state lock poisoned");
            match providers.entry(key) {
                Entry::Vacant(entry) => {
                    entry.insert(ProviderRecord {
                        provider: provider.clone(),
                        principal: principal.clone(),
                        inventory_scope: ProviderInventoryScope::Full,
                        inventory: None,
                        reconciled_snapshot_generation: None,
                        pending_stale_workers: Vec::new(),
                    });
                    provider.clone()
                }
                Entry::Occupied(mut entry) => {
                    let current = entry.get();
                    if current.principal != *principal {
                        return Err(Self::rejection(
                            "AUTHORITY_DENIED",
                            "Provider session belongs to another authenticated Principal",
                        ));
                    }
                    if provider.identity.generation < current.provider.identity.generation {
                        return Err(Self::rejection(
                            "STALE_GENERATION",
                            "provider session generation is stale",
                        ));
                    }
                    if provider.identity.generation == current.provider.identity.generation {
                        if current.provider == provider {
                            return Ok(current.provider.clone());
                        }
                        if current.provider.state != semantic::ProviderState::Unavailable
                            && provider.state == semantic::ProviderState::Unavailable
                            && current.provider.capabilities == provider.capabilities
                        {
                            entry.insert(ProviderRecord {
                                provider: provider.clone(),
                                principal: principal.clone(),
                                inventory_scope: ProviderInventoryScope::Full,
                                // A disconnected transport invalidates only
                                // its session-bound evidence. The logical
                                // Provider identity remains unchanged until a
                                // later registration establishes a new session.
                                // 传输断开只会使绑定到该 session 的证据失效。逻辑 Provider 身份保持不变，直到后续注册建立新的 session。
                                inventory: None,
                                reconciled_snapshot_generation: None,
                                pending_stale_workers: Vec::new(),
                            });
                            provider.clone()
                        } else {
                            return Err(Self::rejection(
                                "STALE_GENERATION",
                                "provider session generation cannot change its facts or recover",
                            ));
                        }
                    } else {
                        entry.insert(ProviderRecord {
                            provider: provider.clone(),
                            principal: principal.clone(),
                            inventory_scope: ProviderInventoryScope::Full,
                            // A reconnect invalidates evidence tied to the old
                            // transport session, not resource or snapshot numbers.
                            // 重新连接会使绑定到旧传输 session 的证据失效，不会改变资源编号或快照编号。
                            inventory: None,
                            reconciled_snapshot_generation: None,
                            pending_stale_workers: Vec::new(),
                        });
                        provider.clone()
                    }
                }
            }
        };
        self.publish_semantic_event_in(
            &context.namespace,
            registered.identity.clone(),
            "provider.registered",
            "cyrene.provider.v1",
            Vec::new(),
        );
        Ok(registered)
    }

    fn publish_inventory(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        snapshot: semantic::ProviderSnapshot,
    ) -> Result<semantic::ProviderSnapshot, semantic::Rejection> {
        self.validate_context(context)?;
        snapshot
            .validate()
            .map_err(|error| Self::rejection(error.reason_code, error.message))?;

        let key = (context.namespace.clone(), snapshot.provider.id.clone());
        {
            let mut providers = self
                .runtime
                .providers
                .lock()
                .expect("provider state lock poisoned");
            let record = providers.get_mut(&key).ok_or_else(|| {
                Self::rejection(
                    "PROVIDER_NOT_FOUND",
                    "provider has not registered this session",
                )
            })?;
            if record.principal != *principal {
                return Err(Self::rejection(
                    "AUTHORITY_DENIED",
                    "Provider inventory caller does not own the registered session",
                ));
            }
            if record.provider.identity != snapshot.provider {
                return Err(Self::rejection(
                    "STALE_GENERATION",
                    "inventory belongs to a stale provider session",
                ));
            }
            if record
                .inventory
                .as_ref()
                .is_some_and(|current| snapshot.snapshot_generation <= current.snapshot_generation)
            {
                return Err(Self::rejection(
                    "STALE_GENERATION",
                    "inventory snapshot generation must advance",
                ));
            }
            record.inventory = Some(snapshot.clone());
            record.reconciled_snapshot_generation = None;
        }
        self.publish_semantic_event_in(
            &context.namespace,
            snapshot.provider.clone(),
            "provider.inventory.observed",
            "cyrene.provider.v1",
            Vec::new(),
        );
        Ok(snapshot)
    }

    fn reconcile_provider(
        &self,
        context: &AuthorityCallContext,
        principal: &semantic::Principal,
        provider: &semantic::Identity,
    ) -> Result<ProviderReconcileResult, semantic::Rejection> {
        self.validate_context(context)?;
        let key = (context.namespace.clone(), provider.id.clone());
        let (provider, snapshot_generation, snapshot, already_reconciled, resource_facts_only) = {
            let mut providers = self
                .runtime
                .providers
                .lock()
                .expect("provider state lock poisoned");
            let record = providers.get_mut(&key).ok_or_else(|| {
                Self::rejection(
                    "PROVIDER_NOT_FOUND",
                    "provider is unknown in this namespace",
                )
            })?;
            if record.principal != *principal {
                return Err(Self::rejection(
                    "AUTHORITY_DENIED",
                    "Provider reconciliation caller does not own the registered session",
                ));
            }
            if record.provider.identity != *provider {
                return Err(Self::rejection(
                    "STALE_GENERATION",
                    "reconciliation targets a stale provider session",
                ));
            }
            let inventory_expired = record
                .inventory
                .as_ref()
                .is_some_and(|snapshot| snapshot.expires_at_unix_ms <= Self::now_unix_ms());
            if inventory_expired {
                record.inventory = None;
                record.reconciled_snapshot_generation = None;
            }
            let snapshot_generation = record
                .inventory
                .as_ref()
                .map_or(0, |snapshot| snapshot.snapshot_generation);
            if record.inventory.is_none()
                && record.provider.state != semantic::ProviderState::Unavailable
                && !inventory_expired
            {
                return Err(Self::rejection(
                    "PROVIDER_INVENTORY_MISSING",
                    "provider has no current inventory",
                ));
            }
            (
                record.provider.identity.clone(),
                snapshot_generation,
                record.inventory.clone(),
                record.reconciled_snapshot_generation == Some(snapshot_generation)
                    && record.pending_stale_workers.is_empty(),
                record.inventory_scope == ProviderInventoryScope::ResourceFactsOnly,
            )
        };
        if already_reconciled {
            return Ok(ProviderReconcileResult {
                provider,
                snapshot_generation,
                actions: vec![ProviderReconcileAction::Noop],
            });
        }

        if resource_facts_only {
            let mut actions = snapshot
                .as_ref()
                .map(|snapshot| {
                    snapshot
                        .resources
                        .iter()
                        .map(|resource| {
                            ProviderReconcileAction::RefreshResource(resource.identity.clone())
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if actions.is_empty() {
                actions.push(ProviderReconcileAction::Noop);
            }
            self.runtime
                .providers
                .lock()
                .expect("provider state lock poisoned")
                .get_mut(&key)
                .expect("provider was checked above")
                .reconciled_snapshot_generation = Some(snapshot_generation);
            self.publish_semantic_event_in(
                &context.namespace,
                provider.clone(),
                "provider.reconciled",
                "cyrene.provider.v1",
                Vec::new(),
            );
            return Ok(ProviderReconcileResult {
                provider,
                snapshot_generation,
                actions,
            });
        }

        let mut actions = snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .resources
                    .iter()
                    .map(|resource| {
                        ProviderReconcileAction::RefreshResource(resource.identity.clone())
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let observed_workers = snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .workers
                    .iter()
                    .map(|worker| worker.identity.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let recorded_worker_names = self
            .runtime
            .workers
            .lock()
            .expect("worker scope lock poisoned")
            .iter()
            .filter(|(object, _)| object.namespace == context.namespace)
            .map(|(_, instance_name)| instance_name.clone())
            .collect::<Vec<_>>();
        let recorded_workers = self
            .runtime
            .instances
            .lock()
            .expect("instance lock poisoned")
            .iter()
            .filter(|(instance_name, _)| recorded_worker_names.contains(instance_name))
            .filter_map(|(_, process)| process.semantic_worker.clone())
            .filter(|worker| {
                worker.provider.id == provider.id
                    && !matches!(
                        worker.state,
                        semantic::WorkerState::Stopped
                            | semantic::WorkerState::Failed
                            | semantic::WorkerState::Lost
                    )
            })
            .collect::<Vec<_>>();
        for worker in recorded_workers {
            let missing_from_provider = !observed_workers.contains(&worker.identity);
            let lease_is_valid = self.lease_for(context, &worker.lease).is_ok_and(|lease| {
                lease.state == LeaseState::Active && lease.holder == worker.identity
            });
            if missing_from_provider || !lease_is_valid {
                actions.extend(self.mark_worker_lost(
                    context,
                    &worker.identity,
                    if missing_from_provider && snapshot.is_some() {
                        "WORKER_MISSING_FROM_PROVIDER"
                    } else if missing_from_provider {
                        "PROVIDER_UNAVAILABLE"
                    } else {
                        "WORKER_LEASE_INVALID"
                    },
                )?);
            }
        }
        let mut stale_workers = Vec::new();
        for observed in observed_workers {
            if !self
                .runtime
                .workers
                .lock()
                .expect("worker scope lock poisoned")
                .contains_key(&context.object_ref(observed.clone()))
            {
                stale_workers.push(observed);
            }
        }
        {
            let mut providers = self
                .runtime
                .providers
                .lock()
                .expect("provider state lock poisoned");
            let record = providers.get_mut(&key).expect("provider was checked above");
            for worker in stale_workers {
                if !record
                    .pending_stale_workers
                    .iter()
                    .any(|(candidate, _)| candidate == &worker)
                {
                    record
                        .pending_stale_workers
                        .push((worker, snapshot_generation));
                }
            }
            actions.extend(
                record.pending_stale_workers.iter().map(|(worker, _)| {
                    ProviderReconcileAction::TerminateStaleWorker(worker.clone())
                }),
            );
        }
        if actions.is_empty() {
            actions.push(ProviderReconcileAction::Noop);
        }
        let converged = !actions
            .iter()
            .any(|action| matches!(action, ProviderReconcileAction::TerminateStaleWorker(_)));
        if converged {
            self.runtime
                .providers
                .lock()
                .expect("provider state lock poisoned")
                .get_mut(&key)
                .expect("provider was checked above")
                .reconciled_snapshot_generation = Some(snapshot_generation);
        }
        self.publish_semantic_event_in(
            &context.namespace,
            provider.clone(),
            "provider.reconciled",
            "cyrene.provider.v1",
            Vec::new(),
        );
        Ok(ProviderReconcileResult {
            provider,
            snapshot_generation,
            actions,
        })
    }
}
