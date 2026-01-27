#!/bin/bash
set -e

# Add replication entry to pg_hba.conf
echo "host replication replicator 0.0.0.0/0 scram-sha-256" >> "$PGDATA/pg_hba.conf"

# Reload PostgreSQL configuration
pg_ctl reload -D "$PGDATA"
