-- Create replication user
-- Password is provided via the POSTGRES_REPLICATION_PASSWORD env var rendered
-- through psql variable substitution. The Postgres entrypoint exports all
-- POSTGRES_* env vars; we forward it via PSQL_OPTIONS / -v to make it visible
-- here as :'replication_password'.
\set replication_user `echo "$POSTGRES_REPLICATION_USER"`
\set replication_password `echo "$POSTGRES_REPLICATION_PASSWORD"`
CREATE USER :"replication_user" WITH REPLICATION ENCRYPTED PASSWORD :'replication_password';

-- Grant necessary permissions for replication
GRANT USAGE ON SCHEMA public TO :"replication_user";
GRANT SELECT ON ALL TABLES IN SCHEMA public TO :"replication_user";
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO :"replication_user";

-- Create replication slot for the replica
SELECT pg_create_physical_replication_slot('replica_slot', true);

-- Grant permissions to the application user for schema and future tables
-- This ensures the app user can create tables when it starts up.
\set app_user `echo "$POSTGRES_USER"`
\set app_db   `echo "$POSTGRES_DB"`
GRANT ALL PRIVILEGES ON SCHEMA public TO :"app_user";
GRANT ALL PRIVILEGES ON DATABASE :"app_db" TO :"app_user";
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL PRIVILEGES ON TABLES TO :"app_user";
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL PRIVILEGES ON SEQUENCES TO :"app_user";

-- Note: Tables will be created by the application on first startup via postgres.rs