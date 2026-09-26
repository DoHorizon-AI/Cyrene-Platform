// Workspace data-plane foundation. This review template creates private networking,
// a VNet-integrated PostgreSQL server, a private Key Vault, and a staged ACA environment.
// 工作区数据层基础设施审阅模板；不包含数据库角色、应用发布或密钥值。

targetScope = 'resourceGroup'

@description('Azure region for the VNet, PostgreSQL, Key Vault, and staged ACA environment. East Asia is the current Product region; verify capacity before deployment.')
param location string = 'eastasia'

@minLength(3)
@maxLength(20)
@description('Lowercase ASCII letters, digits, and hyphens used as a resource-name prefix.')
param resourcePrefix string

@description('Globally unique Key Vault name: 3-24 lowercase letters, digits, and hyphens.')
@minLength(3)
@maxLength(24)
param keyVaultName string

@description('VNet CIDR. Check for overlap with every existing Azure, VPN, ExpressRoute, and on-premises network.')
param virtualNetworkAddressPrefix string

@description('Dedicated ACA workload-profile subnet CIDR. Use at least /27; /23 is reserved here for growth and transition headroom.')
param containerAppsSubnetPrefix string

@description('Dedicated PostgreSQL subnet CIDR. Azure minimum is /28; this design uses /24 for headroom and HA capacity.')
param postgresSubnetPrefix string

@description('Dedicated Key Vault private-endpoint subnet CIDR. Use at least /27 and keep this subnet separate from delegated subnets.')
param privateEndpointSubnetPrefix string

@description('PostgreSQL administrator login for initial bootstrap only. Later migrations use a separate controlled principal; never assign this login to a request-serving app.')
param postgresAdministratorLogin string = 'cyreneBootstrapAdmin'

@secure()
@description('Supply at deploy time through an approved secret mechanism. This value is not emitted as an output.')
param postgresAdministratorPassword string

@description('PostgreSQL Flexible Server vCore SKU. The default is a 2-vCore General Purpose Intel v5 size listed in East Asia.')
param postgresSkuName string = 'Standard_D2ds_v5'

@description('Database shared by Directory and Device Authorization schemas so future generation fencing can use one PostgreSQL transaction.')
param postgresDatabaseName string = 'cyrene_workspace'

@description('Enable zone-redundant capacity for the staged Container Apps environment only after verifying East Asia support and quota.')
param containerAppsZoneRedundant bool = false

@allowed([
  'Disabled'
  'SameZone'
  'ZoneRedundant'
])
@description('Production default is SameZone. Confirm current region capacity before selecting ZoneRedundant.')
param postgresHighAvailabilityMode string = 'SameZone'

@minValue(7)
@maxValue(35)
@description('PostgreSQL point-in-time restore backup retention in days.')
param postgresBackupRetentionDays int = 14

@minValue(32)
@description('Initial PostgreSQL Premium storage allocation in GB. Storage auto-grow is enabled.')
param postgresStorageSizeGB int = 64

var normalizedPrefix = toLower(resourcePrefix)
var uniqueSuffix = uniqueString(resourceGroup().id, normalizedPrefix)
var postgresServerName = '${normalizedPrefix}-pg-${uniqueSuffix}'
var containerAppsEnvironmentName = '${normalizedPrefix}-data-cae'
var tags = {
  component: 'workspace-data'
  managedBy: 'bicep'
  lifecycle: 'review-only-template'
}

resource workspaceVnet 'Microsoft.Network/virtualNetworks@2024-05-01' = {
  name: '${normalizedPrefix}-data-vnet'
  location: location
  tags: tags
  properties: {
    addressSpace: {
      addressPrefixes: [
        virtualNetworkAddressPrefix
      ]
    }
  }
}

resource containerAppsSubnet 'Microsoft.Network/virtualNetworks/subnets@2024-05-01' = {
  parent: workspaceVnet
  name: 'snet-containerapps'
  properties: {
    addressPrefix: containerAppsSubnetPrefix
    delegations: [
      {
        name: 'container-apps'
        properties: {
          serviceName: 'Microsoft.App/environments'
        }
      }
    ]
  }
}

resource postgresSubnet 'Microsoft.Network/virtualNetworks/subnets@2024-05-01' = {
  parent: workspaceVnet
  name: 'snet-postgres'
  properties: {
    addressPrefix: postgresSubnetPrefix
    delegations: [
      {
        name: 'postgres-flexible-server'
        properties: {
          serviceName: 'Microsoft.DBforPostgreSQL/flexibleServers'
        }
      }
    ]
  }
}

resource privateEndpointSubnet 'Microsoft.Network/virtualNetworks/subnets@2024-05-01' = {
  parent: workspaceVnet
  name: 'snet-private-endpoints'
  properties: {
    addressPrefix: privateEndpointSubnetPrefix
  }
}

resource postgresPrivateDnsZone 'Microsoft.Network/privateDnsZones@2024-06-01' = {
  name: 'private.postgres.database.azure.com'
  location: 'global'
  tags: tags
}

resource postgresPrivateDnsLink 'Microsoft.Network/privateDnsZones/virtualNetworkLinks@2024-06-01' = {
  parent: postgresPrivateDnsZone
  name: '${normalizedPrefix}-data-vnet-link'
  location: 'global'
  properties: {
    registrationEnabled: false
    virtualNetwork: {
      id: workspaceVnet.id
    }
  }
}

resource keyVaultPrivateDnsZone 'Microsoft.Network/privateDnsZones@2024-06-01' = {
  name: 'privatelink.vaultcore.azure.net'
  location: 'global'
  tags: tags
}

resource keyVaultPrivateDnsLink 'Microsoft.Network/privateDnsZones/virtualNetworkLinks@2024-06-01' = {
  parent: keyVaultPrivateDnsZone
  name: '${normalizedPrefix}-data-vnet-link'
  location: 'global'
  properties: {
    registrationEnabled: false
    virtualNetwork: {
      id: workspaceVnet.id
    }
  }
}

resource keyVault 'Microsoft.KeyVault/vaults@2025-05-01' = {
  name: keyVaultName
  location: location
  tags: tags
  properties: {
    tenantId: tenant().tenantId
    sku: {
      family: 'A'
      name: 'standard'
    }
    enableRbacAuthorization: true
    enableSoftDelete: true
    softDeleteRetentionInDays: 90
    enablePurgeProtection: true
    publicNetworkAccess: 'Disabled'
    networkAcls: {
      bypass: 'None'
      defaultAction: 'Deny'
    }
  }
}

resource keyVaultPrivateEndpoint 'Microsoft.Network/privateEndpoints@2025-07-01' = {
  name: '${normalizedPrefix}-kv-private-endpoint'
  location: location
  tags: tags
  properties: {
    subnet: {
      id: privateEndpointSubnet.id
    }
    privateLinkServiceConnections: [
      {
        name: '${normalizedPrefix}-key-vault'
        properties: {
          privateLinkServiceId: keyVault.id
          groupIds: [
            'vault'
          ]
        }
      }
    ]
  }
}

resource keyVaultPrivateDnsZoneGroup 'Microsoft.Network/privateEndpoints/privateDnsZoneGroups@2024-05-01' = {
  parent: keyVaultPrivateEndpoint
  name: 'default'
  properties: {
    privateDnsZoneConfigs: [
      {
        name: 'key-vault'
        properties: {
          privateDnsZoneId: keyVaultPrivateDnsZone.id
        }
      }
    ]
  }
}

resource stagedContainerAppsEnvironment 'Microsoft.App/managedEnvironments@2024-03-01' = {
  name: containerAppsEnvironmentName
  location: location
  tags: tags
  properties: {
    appLogsConfiguration: {
      destination: 'none'
    }
    vnetConfiguration: {
      infrastructureSubnetId: containerAppsSubnet.id
      internal: false
    }
    workloadProfiles: [
      {
        name: 'Consumption'
        workloadProfileType: 'Consumption'
      }
    ]
    zoneRedundant: containerAppsZoneRedundant
  }
}

resource postgresServer 'Microsoft.DBforPostgreSQL/flexibleServers@2025-08-01' = {
  name: postgresServerName
  location: location
  tags: tags
  sku: {
    name: postgresSkuName
    tier: 'GeneralPurpose'
  }
  properties: {
    administratorLogin: postgresAdministratorLogin
    administratorLoginPassword: postgresAdministratorPassword
    version: '17'
    authConfig: {
      activeDirectoryAuth: 'Disabled'
      passwordAuth: 'Enabled'
    }
    backup: {
      backupRetentionDays: postgresBackupRetentionDays
      geoRedundantBackup: 'Disabled'
    }
    highAvailability: {
      mode: postgresHighAvailabilityMode
    }
    network: {
      delegatedSubnetResourceId: postgresSubnet.id
      privateDnsZoneArmResourceId: postgresPrivateDnsZone.id
    }
    storage: {
      storageSizeGB: postgresStorageSizeGB
      autoGrow: 'Enabled'
      type: 'Premium_LRS'
    }
  }
}

resource workspaceDatabase 'Microsoft.DBforPostgreSQL/flexibleServers/databases@2025-08-01' = {
  parent: postgresServer
  name: postgresDatabaseName
  properties: {
    charset: 'UTF8'
    collation: 'en_US.utf8'
  }
}

resource postgresTlsMinimum 'Microsoft.DBforPostgreSQL/flexibleServers/configurations@2025-08-01' = {
  parent: postgresServer
  name: 'ssl_min_protocol_version'
  properties: {
    source: 'user-override'
    value: 'TLSv1.3'
  }
}

resource postgresRequireSecureTransport 'Microsoft.DBforPostgreSQL/flexibleServers/configurations@2025-08-01' = {
  parent: postgresServer
  name: 'require_secure_transport'
  properties: {
    source: 'user-override'
    value: 'ON'
  }
}

resource workspaceDirectoryReaderIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${normalizedPrefix}-directory-reader-${uniqueSuffix}'
  location: location
  tags: tags
}

resource workspaceDirectoryOperatorIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${normalizedPrefix}-directory-operator-${uniqueSuffix}'
  location: location
  tags: tags
}

resource workspaceDeviceRegistrarIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${normalizedPrefix}-device-registrar-${uniqueSuffix}'
  location: location
  tags: tags
}

resource workspaceDeviceAuthorizationIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${normalizedPrefix}-device-authorization-${uniqueSuffix}'
  location: location
  tags: tags
}

resource workspaceMigrationIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${normalizedPrefix}-db-migration-${uniqueSuffix}'
  location: location
  tags: tags
}

output postgresServerId string = postgresServer.id
output postgresServerName string = postgresServer.name
output postgresServerFqdn string = '${postgresServer.name}.postgres.database.azure.com'
output postgresDatabaseName string = workspaceDatabase.name
output postgresServerSubnetId string = postgresSubnet.id
output postgresPrivateDnsZoneId string = postgresPrivateDnsZone.id
output keyVaultId string = keyVault.id
output keyVaultUri string = 'https://${keyVault.name}.${environment().suffixes.keyvaultDns}/'
output keyVaultPrivateEndpointId string = keyVaultPrivateEndpoint.id
output containerAppsEnvironmentId string = stagedContainerAppsEnvironment.id
output containerAppsSubnetId string = containerAppsSubnet.id
output managedIdentityIds object = {
  workspaceDirectoryReader: workspaceDirectoryReaderIdentity.id
  workspaceDirectoryOperator: workspaceDirectoryOperatorIdentity.id
  workspaceDeviceRegistrar: workspaceDeviceRegistrarIdentity.id
  workspaceDeviceAuthorization: workspaceDeviceAuthorizationIdentity.id
  workspaceMigrationRunner: workspaceMigrationIdentity.id
}
output managedIdentityPrincipalIds object = {
  workspaceDirectoryReader: workspaceDirectoryReaderIdentity.properties.principalId
  workspaceDirectoryOperator: workspaceDirectoryOperatorIdentity.properties.principalId
  workspaceDeviceRegistrar: workspaceDeviceRegistrarIdentity.properties.principalId
  workspaceDeviceAuthorization: workspaceDeviceAuthorizationIdentity.properties.principalId
  workspaceMigrationRunner: workspaceMigrationIdentity.properties.principalId
}
