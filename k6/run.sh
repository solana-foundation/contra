#!/bin/bash

# Contra K6 Load Test Runner
# Usage: ./run.sh [local|cloud]

set -e

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Environment selection
ENV="${1:-cloud}"

# Set RPC URL based on environment
if [ "$ENV" = "local" ]; then
    RPC_URL="http://localhost:8899"
    echo -e "${BLUE}📍 Using LOCAL environment${NC}"
elif [ "$ENV" = "cloud" ]; then
    RPC_URL="https://write.onlyoncontra.xyz"
    echo -e "${GREEN}☁️  Using CLOUD environment${NC}"
else
    echo "Usage: ./run.sh [local|cloud]"
    echo "  local - Run against localhost:8899"
    echo "  cloud - Run against write.onlyoncontra.xyz"
    exit 1
fi

echo -e "${YELLOW}🚀 Contra K6 Load Test - Send Transaction${NC}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo -e "RPC URL: ${BLUE}${RPC_URL}${NC}"
echo ""

# Check if node_modules exists
if [ ! -d "node_modules" ]; then
    echo "📦 Installing dependencies..."
    npm install
fi

# Build TypeScript
echo "🔨 Building TypeScript..."
npm run build

# Check if k6 is installed
if ! command -v k6 &> /dev/null; then
    echo "❌ k6 is not installed!"
    echo "Install with: brew install k6 (macOS) or snap install k6 (Linux)"
    exit 1
fi

echo ""
echo "▶️  Starting load test..."
echo ""

# Run k6 test
if [ "$ENV" = "cloud" ]; then
    # Run on k6 cloud
    k6 cloud run \
        --tag environment="$ENV" \
        -e RPC_URL="$RPC_URL" \
        dist/send-transaction.js
else
    # Run locally
    k6 run \
        --summary-trend-stats="avg,min,med,max,p(95),p(99)" \
        --tag environment="$ENV" \
        -e RPC_URL="$RPC_URL" \
        dist/send-transaction.js
fi

echo ""
echo -e "${GREEN}✅ Test completed!${NC}"