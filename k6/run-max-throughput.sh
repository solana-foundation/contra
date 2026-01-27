#!/bin/bash

# Contra K6 Maximum Throughput Test
# Usage: ./run-max-throughput.sh [local|cloud]

set -e

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
RED='\033[0;31m'
NC='\033[0m'

# Environment selection
ENV="${1:-local}"

# Set RPC URL based on environment
if [ "$ENV" = "local" ]; then
    RPC_URL="http://localhost:8899"
    echo -e "${BLUE}📍 Using LOCAL environment${NC}"
elif [ "$ENV" = "cloud" ]; then
    RPC_URL="https://write.onlyoncontra.xyz"
    echo -e "${GREEN}☁️  Using CLOUD environment${NC}"
else
    echo "Usage: ./run-max-throughput.sh [local|cloud]"
    echo "  local - Run against localhost:8899"
    echo "  cloud - Run against write.onlyoncontra.xyz"
    exit 1
fi

# Check if node_modules exists
if [ ! -d "node_modules" ]; then
    echo "📦 Installing dependencies..."
    pnpm install
fi

# Add max-throughput to webpack if needed
if ! grep -q "max-throughput" webpack.config.js; then
    echo "📝 Adding max-throughput to webpack config..."
    sed -i "s/'send-transaction': '.\/src\/send-transaction.ts',/'send-transaction': '.\/src\/send-transaction.ts',\n    'max-throughput': '.\/src\/max-throughput.ts',/" webpack.config.js
fi

# Build TypeScript
echo "🔨 Building TypeScript..."
pnpm run build

# Check if k6 is installed
if ! command -v k6 &> /dev/null; then
    echo "❌ k6 is not installed!"
    echo "Install with: brew install k6 (macOS) or snap install k6 (Linux)"
    exit 1
fi

echo ""
echo "▶️  Starting maximum throughput test..."
echo ""

# Run k6 test
if [ "$ENV" = "cloud" ]; then
    # Run on k6 cloud
    k6 cloud run \
        --tag environment="$ENV" \
        --tag test_type="max_throughput" \
        -e RPC_URL="$RPC_URL" \
        dist/max-throughput.js
else
    # Run locally with detailed stats
    k6 run \
        --summary-trend-stats="avg,min,med,max,p(90),p(95),p(99)" \
        --summary-time-unit="ms" \
        --tag environment="$ENV" \
        --tag test_type="max_throughput" \
        -e RPC_URL="$RPC_URL" \
        dist/max-throughput.js
fi

echo ""
echo -e "${GREEN}✅ Maximum throughput test completed!${NC}"
echo ""
echo "Key metrics to observe:"
echo "  • http_reqs............: Total requests and requests/second"
echo "  • http_req_duration....: Response time percentiles"
echo "  • send_success.........: Success rate percentage"
echo "  • vus_max.............: Maximum concurrent users reached"
echo ""