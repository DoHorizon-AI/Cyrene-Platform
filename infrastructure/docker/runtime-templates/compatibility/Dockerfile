ARG ASTRBOT_PLATFORMS=onebot,weixin_oc,wecom,wecom_ai_bot,discord,satori,webchat
ARG GIT_SHA=unknown

FROM mcr.microsoft.com/dotnet/sdk:10.0 AS dotnet-build
ARG ASTRBOT_PLATFORMS
WORKDIR /src

COPY NuGet.Config global.json Directory.Build.props ./
COPY scripts/dotnet_restore_with_fallback.sh /usr/local/bin/dotnet-restore-with-fallback
COPY src/AstrBot.DotNetHost/AstrBot.DotNetHost.csproj src/AstrBot.DotNetHost/
COPY src/IrisHistoryImporter/IrisHistoryImporter.csproj src/IrisHistoryImporter/
COPY src/AstrBot.DatabaseMigrator/AstrBot.DatabaseMigrator.csproj src/AstrBot.DatabaseMigrator/
COPY astrbot/dashboard/internalized_plugin_configs/astrbot_plugin_iris_chat_memory/_conf_schema.json \
    astrbot/dashboard/internalized_plugin_configs/astrbot_plugin_iris_chat_memory/_conf_schema.json
RUN sh /usr/local/bin/dotnet-restore-with-fallback \
    src/IrisHistoryImporter/IrisHistoryImporter.csproj \
    src/AstrBot.DatabaseMigrator/AstrBot.DatabaseMigrator.csproj

COPY src/AstrBot.DotNetHost/ src/AstrBot.DotNetHost/
COPY src/IrisHistoryImporter/ src/IrisHistoryImporter/
COPY src/AstrBot.DatabaseMigrator/ src/AstrBot.DatabaseMigrator/
RUN platforms_msbuild="$(printf '%s' "${ASTRBOT_PLATFORMS}" | sed 's/,/%2C/g')" \
    && dotnet publish src/AstrBot.DotNetHost/AstrBot.DotNetHost.csproj \
        --configuration Release \
        --output /app/host \
        --no-restore \
        -p:DebugSymbols=false \
        -p:DebugType=None \
        -p:AstrBotPlatforms="${platforms_msbuild}" \
    && dotnet publish src/IrisHistoryImporter/IrisHistoryImporter.csproj \
        --configuration Release \
        --output /app/tools \
        --no-restore \
        -p:DebugSymbols=false \
        -p:DebugType=None \
    && dotnet publish src/AstrBot.DatabaseMigrator/AstrBot.DatabaseMigrator.csproj \
        --configuration Release \
        --output /app/migrator \
        --no-restore \
        -p:DebugSymbols=false \
        -p:DebugType=None

FROM node:22-bookworm-slim AS dashboard-build
WORKDIR /src/dashboard
COPY scripts/apt_install_with_fallback.sh /usr/local/bin/apt-install-with-fallback
RUN bash /usr/local/bin/apt-install-with-fallback ca-certificates \
    && corepack enable
COPY dashboard/package.json dashboard/pnpm-lock.yaml dashboard/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY dashboard/ ./
RUN pnpm build

FROM node:22-bookworm-slim AS iris-build
WORKDIR /src/iris
COPY scripts/apt_install_with_fallback.sh /usr/local/bin/apt-install-with-fallback
RUN bash /usr/local/bin/apt-install-with-fallback ca-certificates \
    && corepack enable
COPY astrbot/core/memory/iris/web/frontend/package.json \
    astrbot/core/memory/iris/web/frontend/pnpm-lock.yaml \
    astrbot/core/memory/iris/web/frontend/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY astrbot/core/memory/iris/web/frontend/ ./
RUN pnpm build

FROM mcr.microsoft.com/dotnet/aspnet:10.0 AS runtime-base
ARG APP_UID=1654
WORKDIR /AstrBot
ENV ASPNETCORE_ENVIRONMENT=Production \
    ASPNETCORE_URLS=http://+:15031 \
    DOTNET_EnableDiagnostics=0 \
    Runtime__Mode=Production \
    DeveloperCompatibility__PythonPluginsEnabled=false \
    APP_UID=${APP_UID}
COPY scripts/apt_install_with_fallback.sh /usr/local/bin/apt-install-with-fallback
RUN bash /usr/local/bin/apt-install-with-fallback ca-certificates curl \
    && mkdir -p /AstrBot/data \
    && chown -R "${APP_UID}:0" /AstrBot/data \
    && touch /AstrBot/.astrbot
COPY --from=dotnet-build /app/host /app/dotnethost
COPY --from=dotnet-build /app/host/appsettings.json /AstrBot/appsettings.json
COPY --from=dotnet-build /app/host/appsettings.Development.json /AstrBot/appsettings.Development.json
COPY --from=dotnet-build /app/migrator /app/migrator
COPY --from=dashboard-build /src/dashboard/dist /AstrBot/wwwroot
COPY --from=iris-build /pages/iris/index.html /AstrBot/wwwroot/iris/index.html
COPY pyproject.toml /AstrBot/pyproject.toml
COPY scripts/deploy.sh /app/deploy/deploy.sh
COPY config/provider-capabilities.json /app/manifest/provider-capabilities.json
COPY config/supported-platforms.json /app/manifest/supported-platforms.json
EXPOSE 15031
USER $APP_UID
ENTRYPOINT ["dotnet", "/app/dotnethost/AstrBot.DotNetHost.dll"]

FROM runtime-base AS developer
ARG GIT_SHA
ENV ASPNETCORE_ENVIRONMENT=Development \
    ASTRBOT_BUILD_SHA=${GIT_SHA} \
    Build__CommitSha=${GIT_SHA} \
    Runtime__Mode=Development \
    DeveloperCompatibility__PythonPluginsEnabled=false \
    HostRuntimeOptions__EnableWorkerSupervision=false \
    PythonWorker__EnableCompatibilityProxy=false \
    PythonWorker__EnableHealthProbe=false
LABEL org.opencontainers.image.revision="${GIT_SHA}"
COPY --from=dotnet-build /app/tools /app/tools

FROM runtime-base AS production
ARG GIT_SHA
ENV ASTRBOT_BUILD_SHA=${GIT_SHA} \
    Build__CommitSha=${GIT_SHA}
LABEL org.opencontainers.image.revision="${GIT_SHA}"

FROM production AS production-importer
COPY --from=dotnet-build /app/tools /app/tools
USER $APP_UID
ENTRYPOINT ["dotnet", "/app/tools/IrisHistoryImporter.dll"]

FROM mcr.microsoft.com/dotnet/aspnet:10.0 AS capability-worker-build
WORKDIR /tmp/capability-worker
COPY scripts/apt_install_with_fallback.sh /usr/local/bin/apt-install-with-fallback
RUN bash /usr/local/bin/apt-install-with-fallback python3 python3-venv
COPY --from=ghcr.io/astral-sh/uv:0.11.25 /uv /usr/local/bin/uv
COPY python/capability_worker/pyproject.toml python/capability_worker/uv.lock python/capability_worker/README.md ./
COPY .docker/capability-site-packages /tmp/capability-site-packages
RUN python3 -m venv --copies /opt/astrbot-capability-worker \
    && if find /tmp/capability-site-packages -type f ! -name .gitkeep -print -quit | grep -q .; then \
        site_packages="$(/opt/astrbot-capability-worker/bin/python -c 'import sysconfig; print(sysconfig.get_paths()["purelib"])')"; \
        cp -a /tmp/capability-site-packages/. "${site_packages}/"; \
    else \
        UV_PROJECT_ENVIRONMENT=/opt/astrbot-capability-worker \
        UV_PYTHON_DOWNLOADS=never \
        UV_LINK_MODE=copy \
        sh -ec 'for attempt in 1 2 3; do \
            uv sync --frozen --no-dev --no-install-project && exit 0; \
            if [ "$attempt" -eq 3 ]; then exit 1; fi; \
            sleep $((attempt * 10)); \
        done'; \
    fi

FROM production AS production-capabilities
USER root
RUN bash /usr/local/bin/apt-install-with-fallback python3 \
    && mkdir -p /app/capability-worker /AstrBot/data
COPY --from=capability-worker-build /opt/astrbot-capability-worker /opt/astrbot-capability-worker
COPY python/capability_worker/src/capability_worker/ /app/capability-worker/capability_worker/
RUN chown -R "${APP_UID}:0" /app/capability-worker /AstrBot/data
ENV PythonCapabilities__Enabled=false \
    PythonCapabilities__Executable=/opt/astrbot-capability-worker/bin/python \
    PythonCapabilities__Arguments__0=-m \
    PythonCapabilities__Arguments__1=capability_worker \
    PythonCapabilities__WorkingDirectory=/app/capability-worker
USER $APP_UID

FROM runtime-base AS production-media
USER root
RUN bash /usr/local/bin/apt-install-with-fallback ffmpeg
ARG GIT_SHA
ENV ASTRBOT_BUILD_SHA=${GIT_SHA} \
    Build__CommitSha=${GIT_SHA}
LABEL org.opencontainers.image.revision="${GIT_SHA}"
USER $APP_UID

FROM mcr.microsoft.com/dotnet/aspnet:10.0 AS vector-worker-build
WORKDIR /tmp/vector-worker
COPY scripts/apt_install_with_fallback.sh /usr/local/bin/apt-install-with-fallback
RUN bash /usr/local/bin/apt-install-with-fallback python3 python3-venv
COPY --from=ghcr.io/astral-sh/uv:0.11.25 /uv /usr/local/bin/uv
COPY python/vector_worker/pyproject.toml python/vector_worker/uv.lock python/vector_worker/README.md ./
RUN UV_PROJECT_ENVIRONMENT=/opt/astrbot-vector-worker \
    UV_PYTHON_DOWNLOADS=never \
    UV_LINK_MODE=copy \
    uv sync --frozen --no-dev --no-install-project

# Dormant opt-in target: out-of-process FAISS vector worker. Not part of the
# default production image; build this target and set
# ASTRBOT_VECTOR_WORKER_MODE=process to enable it.
FROM production AS production-vector
USER root
COPY --from=vector-worker-build /opt/astrbot-vector-worker /opt/astrbot-vector-worker
COPY python/vector_worker/ /app/vector-worker/vector_worker/
RUN chown -R "${APP_UID}:0" /opt/astrbot-vector-worker /app/vector-worker
ENV ASTRBOT_VECTOR_WORKER_COMMAND="/opt/astrbot-vector-worker/bin/python -m vector_worker" \
    ASTRBOT_VECTOR_WORKER_WORKING_DIRECTORY=/app/vector-worker
USER $APP_UID

FROM python:3.12-slim AS python-compat
ARG GIT_SHA
WORKDIR /AstrBot
ENV PATH=/opt/astrbot-venv/bin:$PATH \
    UV_PROJECT_ENVIRONMENT=/opt/astrbot-venv \
    UV_PYTHON_DOWNLOADS=never \
    UV_LINK_MODE=copy \
    ASTRBOT_ROOT=/AstrBot
LABEL org.opencontainers.image.revision="${GIT_SHA}"
COPY --from=ghcr.io/astral-sh/uv:0.11.25 /uv /usr/local/bin/uv
COPY pyproject.toml uv.lock ./
# Plugin runtime dependencies only. The legacy-storage extra (FAISS) belongs to the
# retired SQLite/FAISS knowledge base, which the .NET host now owns on PostgreSQL.
RUN uv sync --frozen --no-dev --no-install-project
COPY main.py runtime_bootstrap.py requirements.txt ./
COPY astrbot/ ./astrbot/
# astrbot.core.db.vec_db imports the in-repo ``python.vector_worker`` package.
COPY python/ ./python/
COPY --from=dashboard-build /src/dashboard/dist /AstrBot/dashboard-dist
RUN groupadd --gid 65532 astrbot \
    && useradd --uid 65532 --gid 65532 --no-create-home --home-dir /nonexistent astrbot \
    && mkdir -p /AstrBot/data \
    && chown -R 65532:65532 /AstrBot/data
USER 65532:65532
HEALTHCHECK --interval=10s --timeout=5s --start-period=20s --retries=12 \
    CMD python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:6185/api/auth/setup-status', timeout=4).read()"
ENTRYPOINT ["python", "main.py", "--webui-dir", "/AstrBot/dashboard-dist"]

# Keep an unqualified Docker build equivalent to the Python-free production target.
FROM production AS final
