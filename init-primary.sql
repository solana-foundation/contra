-- Create replication user
CREATE USER replicator WITH REPLICATION ENCRYPTED PASSWORD 'repl_password';

-- Grant necessary permissions for replication
GRANT USAGE ON SCHEMA public TO replicator;
GRANT SELECT ON ALL TABLES IN SCHEMA public TO replicator;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO replicator;

-- Create replication slot for the replica
SELECT pg_create_physical_replication_slot('replica_slot', true);

-- Grant permissions to contra user for schema and future tables
-- This ensures contra user can create tables when the application starts
GRANT ALL PRIVILEGES ON SCHEMA public TO contra;
GRANT ALL PRIVILEGES ON DATABASE contra TO contra;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL PRIVILEGES ON TABLES TO contra;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL PRIVILEGES ON SEQUENCES TO contra;

-- Note: Tables will be created by the application on first startup via postgres.rs