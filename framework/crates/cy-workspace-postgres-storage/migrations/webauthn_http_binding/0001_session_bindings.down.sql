-- Drop the isolated session-binding schema. Keep the NOLOGIN app role so an
-- existing external runtime membership is not unexpectedly invalidated.
DROP SCHEMA IF EXISTS cyrene_workspace_webauthn_http_binding CASCADE;
