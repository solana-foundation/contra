#!/usr/bin/env python3
"""
Contra Escrow Program - CU Analysis Report Generator

Analyzes compute unit usage from litesvm integration tests to create
CU usage reports for Solana programs.
"""

import json
import os
import re
import subprocess
import sys
from collections import defaultdict
from dataclasses import dataclass
from datetime import datetime
from typing import Dict, List

# =============================================================================
# CONSTANTS
# =============================================================================

DEFAULT_OUTPUT_FILE = "profiling_report.md"
INTEGRATION_TESTS_DIR = "."
INSTRUCTIONS_FILE = "contra-escrow-program/program/src/instructions.rs"

# Solana Transaction Structure (Max 1232 bytes total)
# Based on Solana documentation and IPv6 MTU minus headers

# Message Header (3 bytes)
MESSAGE_HEADER_SIZE = (
    3  # num_required_signatures + num_readonly_signed + num_readonly_unsigned
)

# Account addresses (32 bytes each + compact array length)
ACCOUNT_PUBKEY_SIZE = 32  # Each account public key
COMPACT_ARRAY_LENGTH = 1  # Compact-u16 for small arrays (1-127 items)

# Recent blockhash (32 bytes)
RECENT_BLOCKHASH_SIZE = 32

# Instructions array header
INSTRUCTIONS_LENGTH = 1  # Compact-u16 for instruction count

# Per instruction
PROGRAM_ID_INDEX = 1  # Program ID account index (u8)
ACCOUNT_INDEXES_LENGTH = 1  # Compact-u16 for account indexes length
INSTRUCTION_DATA_LENGTH = 1  # Compact-u16 for instruction data length

# Per-signature costs (separate from message)
SIGNATURE_SIZE = 64  # Ed25519 signature

# Transaction size limit
MAX_TRANSACTION_SIZE = 1232  # Solana's hard limit

# Data type sizes
PUBKEY_SIZE = 32  # Pubkey size
U8_SIZE = 1  # u8 size
U16_SIZE = 2  # u16 size
U32_SIZE = 4  # u32 size
U64_SIZE = 8  # u64 size
I8_SIZE = 1  # i8 size
I16_SIZE = 2  # i16 size
I32_SIZE = 4  # i32 size
I64_SIZE = 8  # i64 size
BOOL_SIZE = 1  # bool size
OPTION_DISCRIMINATOR_SIZE = 1  # Option<T> discriminator (Some/None)
VEC_LENGTH_SIZE = 4  # Vec<T> length prefix (u32)


# =============================================================================
# DATA MODELS
# =============================================================================


@dataclass
class ProfilingData:
    """Represents profiling data for a single operation."""

    operation: str
    cu_consumed: int
    transaction_size: int


@dataclass
class OperationStats:
    """Represents statistical analysis for an operation type."""

    operation: str
    total_calls: int
    total_cu: int
    mean_cu: float
    min_cu: int
    max_cu: int
    total_tx_size: int
    mean_tx_size: float
    min_tx_size: int
    max_tx_size: int


# =============================================================================
# TRANSACTION SIZE ANALYSIS
# =============================================================================


class TransactionSizeAnalyzer:
    """Analyzes instruction definitions to estimate transaction sizes."""

    def __init__(self):
        self.instruction_sizes = self._parse_instruction_sizes()

    def _parse_instruction_sizes(self) -> Dict[str, int]:
        """Parse instruction definitions and calculate transaction sizes."""
        try:
            with open(INSTRUCTIONS_FILE, "r") as f:
                content = f.read()
        except FileNotFoundError:
            raise FileNotFoundError(
                f"❌ Could not find {INSTRUCTIONS_FILE}. "
                f"Transaction size analysis requires the instruction definitions file."
            )

        instruction_sizes = {}

        # Parse each instruction
        instructions = self._extract_instructions(content)

        for instruction_name, account_info, args in instructions:
            size = self._calculate_instruction_size(account_info, args)
            instruction_sizes[instruction_name] = size

        if not instruction_sizes:
            raise ValueError(
                f"❌ Failed to parse any instruction definitions from {INSTRUCTIONS_FILE}. "
                f"Please check the file format and content."
            )

        return instruction_sizes

    def _extract_instructions(self, content: str) -> List[tuple]:
        """Extract instruction definitions from Rust code."""
        instructions = []

        # Find all instruction definitions - handle both patterns: CreateInstance { } = 0, and AllowMint { } = 1
        instruction_pattern = r"(\w+)\s*\{[^}]*\}\s*=\s*(\d+)"
        matches = re.findall(instruction_pattern, content)

        # Also find the first instruction (CreateInstance) which might not have = 0 explicitly
        first_instruction_pattern = r"enum\s+\w+\s*\{[^}]*?(\w+)\s*\{[^}]*\}"
        first_match = re.search(first_instruction_pattern, content, re.DOTALL)

        if first_match and first_match.group(1) not in [m[0] for m in matches]:
            # Add CreateInstance as discriminator 0
            matches.insert(0, (first_match.group(1), "0"))

        for name, discriminator in matches:
            if name == "EmitEvent":  # Skip internal instruction
                continue
            if name.endswith("Instruction"):  # Skip enum name
                continue

            # Parse accounts with their attributes for this instruction
            account_info = self._parse_instruction_accounts(content, name)

            # Parse instruction arguments
            args = self._parse_instruction_args(content, name)

            instructions.append((name, account_info, args))

        return instructions

    def _parse_instruction_accounts(
        self, content: str, instruction_name: str
    ) -> List[Dict[str, str]]:
        """Parse account definitions for a specific instruction with their attributes."""
        # Find the instruction definition and the previous instruction to bound our search
        instruction_pattern = rf"{instruction_name}\s*{{[^}}]*}}\s*=\s*\d+"
        instruction_match = re.search(instruction_pattern, content, re.DOTALL)

        if not instruction_match:
            return []

        instruction_start = instruction_match.start()

        # Find the end of the previous instruction to limit our backward search
        # Look for patterns like "} = 6," (previous instruction end)
        before_content = content[:instruction_start]
        prev_instruction_pattern = r"}\s*=\s*\d+\s*,"
        prev_instruction_matches = list(
            re.finditer(prev_instruction_pattern, before_content)
        )

        if prev_instruction_matches:
            # Start searching from after the last previous instruction
            search_start = prev_instruction_matches[-1].end()
        else:
            # If no previous instruction, search from enum definition
            enum_match = re.search(r"pub enum \w+\s*{", before_content)
            search_start = enum_match.end() if enum_match else 0

        # Extract the section of content that contains only this instruction's accounts
        instruction_section = content[search_start:instruction_start]

        # Find all #[account(...)] annotations in this section
        account_pattern = r"#\[account\([^)]*\)\]"
        account_matches = re.findall(account_pattern, instruction_section, re.DOTALL)

        accounts = []
        for account_match in account_matches:
            account_info = self._parse_single_account(account_match)
            if account_info:
                accounts.append(account_info)

        return accounts

    def _parse_single_account(self, account_line: str) -> Dict[str, str]:
        """Parse a single #[account(...)] annotation."""
        # Extract content between #[account( and )]
        pattern = r"#\[account\((.*)\)\]"
        match = re.search(pattern, account_line, re.DOTALL)

        if not match:
            return {}

        account_content = match.group(1)
        account_info = {
            "signer": False,
            "writable": False,
            "name": "",
            "description": "",
        }

        # Parse individual attributes
        # Look for signer
        if "signer" in account_content:
            account_info["signer"] = True

        # Look for writable
        if "writable" in account_content:
            account_info["writable"] = True

        # Extract name
        name_match = re.search(r'name\s*=\s*["\']([^"\']+)["\']', account_content)
        if name_match:
            account_info["name"] = name_match.group(1)

        # Extract description
        desc_match = re.search(
            r'description\s*=\s*["\']([^"\']+)["\']', account_content
        )
        if desc_match:
            account_info["description"] = desc_match.group(1)

        return account_info

    def _parse_instruction_args(
        self, content: str, instruction_name: str
    ) -> Dict[str, str]:
        """Parse instruction arguments and their types."""
        # Find the instruction struct definition
        pattern = rf"{instruction_name}\s*\{{([^}}]*)\}}"
        match = re.search(pattern, content, re.DOTALL)

        args = {}
        if match:
            args_content = match.group(1)
            # Parse field definitions like "amount: u64," but ignore lines that start with ///
            lines = args_content.split('\n')
            for line in lines:
                line = line.strip()
                # Skip comments
                if line.startswith('///') or line.startswith('//'):
                    continue
                # Parse field definitions like "amount: u64,"
                field_match = re.match(r"(\w+):\s*([^,\n]+)", line)
                if field_match:
                    field_name = field_match.group(1)
                    field_type = field_match.group(2).strip()
                    # Remove trailing comma if present
                    if field_type.endswith(','):
                        field_type = field_type[:-1].strip()
                    args[field_name] = field_type

        return args

    def _calculate_instruction_size(
        self, account_info: List[Dict[str, str]], args: Dict[str, str]
    ) -> int:
        """Calculate the estimated transaction size for an instruction."""
        # Count signers and total accounts
        signer_count = sum(1 for acc in account_info if acc.get("signer", False))
        account_count = len(account_info)

        # Ensure we have at least one signer (payer) even if not explicitly marked
        if signer_count == 0:
            signer_count = 1

        # Calculate signatures size (separate from message)
        signatures_size = signer_count * SIGNATURE_SIZE

        # Calculate message size
        message_size = 0

        # Message header (3 bytes: num_required_signatures, num_readonly_signed, num_readonly_unsigned)
        message_size += MESSAGE_HEADER_SIZE

        # Account keys array
        message_size += self._compact_array_size(account_count)  # Compact array length
        message_size += account_count * ACCOUNT_PUBKEY_SIZE  # Account public keys

        # Recent blockhash
        message_size += RECENT_BLOCKHASH_SIZE

        # Instructions array (single instruction)
        message_size += self._compact_array_size(
            1
        )  # Instructions array length (1 instruction)
        message_size += PROGRAM_ID_INDEX  # Program ID account index

        # Instruction account indexes
        message_size += self._compact_array_size(
            account_count
        )  # Account indexes length
        message_size += account_count  # Account indexes (u8 each)

        # Instruction data
        instruction_data_size = 1  # Discriminator byte
        for arg_name, arg_type in args.items():
            instruction_data_size += self._calculate_type_size(arg_type)

        message_size += self._compact_array_size(
            instruction_data_size
        )  # Instruction data length
        message_size += instruction_data_size

        # Total transaction size = signatures + message
        total_size = signatures_size + message_size

        # Ensure we don't exceed transaction limit
        return min(total_size, MAX_TRANSACTION_SIZE)

    def _compact_array_size(self, length: int) -> int:
        """Calculate the size needed for a compact array length encoding."""
        # Solana uses compact-u16 encoding for array lengths
        if length < 0x80:  # < 128
            return 1  # Single byte
        elif length < 0x4000:  # < 16384
            return 2  # Two bytes
        else:
            return 3  # Three bytes (for lengths up to 2^21 - 1)

    def _calculate_type_size(self, type_str: str) -> int:
        """Calculate the size of a Rust type string."""
        type_str = type_str.strip()

        # Handle Option<T>
        if type_str.startswith("Option<") and type_str.endswith(">"):
            inner_type = type_str[7:-1]  # Remove 'Option<' and '>'
            return OPTION_DISCRIMINATOR_SIZE + self._calculate_type_size(inner_type)

        # Handle Vec<T>
        if type_str.startswith("Vec<") and type_str.endswith(">"):
            inner_type = type_str[4:-1]  # Remove 'Vec<' and '>'
            # Assume average vec size of 4 elements for estimation
            return VEC_LENGTH_SIZE + (4 * self._calculate_type_size(inner_type))

        # Handle arrays [T; N]
        array_match = re.match(r"\[([^;]+);\s*(\d+)\]", type_str)
        if array_match:
            element_type = array_match.group(1).strip()
            array_length = int(array_match.group(2))
            return array_length * self._calculate_type_size(element_type)

        # Handle primitive types
        type_mappings = {
            "u8": U8_SIZE,
            "u16": U16_SIZE,
            "u32": U32_SIZE,
            "u64": U64_SIZE,
            "i8": I8_SIZE,
            "i16": I16_SIZE,
            "i32": I32_SIZE,
            "i64": I64_SIZE,
            "bool": BOOL_SIZE,
            "Pubkey": PUBKEY_SIZE,
            "pubkey": PUBKEY_SIZE,
        }

        # Check for exact matches first
        if type_str in type_mappings:
            return type_mappings[type_str]

        # Check for partial matches (for cases like "solana_program::pubkey::Pubkey")
        for type_name, size in type_mappings.items():
            if type_name.lower() in type_str.lower():
                return size

        raise ValueError(f"❌ Unknown type: {type_str}")

    def get_transaction_size(self, operation: str) -> int:
        """Get the estimated transaction size for an operation."""
        if operation not in self.instruction_sizes:
            raise KeyError(
                f"❌ Unknown operation '{operation}'. "
                f"Available operations: {list(self.instruction_sizes.keys())}"
            )
        return self.instruction_sizes[operation]


# =============================================================================
# PROFILING DATA COLLECTION
# =============================================================================


class ProfilingCollector:
    """Collects profiling data from integration test runs."""

    def __init__(self):
        self.tx_analyzer = TransactionSizeAnalyzer()

    def run_integration_tests(self) -> List[ProfilingData]:
        """Run integration tests and collect profiling data."""
        print("🧪 Running integration tests to collect profiling data...")

        try:
            # Run integration tests and capture stderr where profiling data is output
            result = subprocess.run(
                [
                    "cargo",
                    "test",
                    "-p",
                    "tests-contra-escrow-program",
                    "--",
                    "--nocapture",
                ],
                cwd=INTEGRATION_TESTS_DIR,
                capture_output=True,
                text=True,
                env={**os.environ, "ENABLE_PROFILING": "1"},
            )

            if result.returncode != 0:
                print(
                    f"⚠️ Tests completed with exit code {result.returncode} (some tests may have failed)"
                )
                # Continue processing - we can still extract profiling data from stdout

        except subprocess.CalledProcessError as e:
            print(f"❌ Failed to run tests: {e}")
            return []
        except FileNotFoundError:
            print(
                f"❌ Could not find cargo or integration tests directory: {INTEGRATION_TESTS_DIR}"
            )
            return []

        # Parse profiling data from both stdout and stderr
        combined_output = result.stdout + result.stderr
        profiling_data = self._parse_profiling_output(combined_output)

        if profiling_data:
            print(f"✅ Collected {len(profiling_data)} profiling data points")
        else:
            print("⚠️  No profiling data found in test output")
            # Show first few lines for debugging
            if combined_output:
                print("📋 First 10 lines of test output:")
                lines = combined_output.split("\n")[:10]
                for i, line in enumerate(lines, 1):
                    print(f"  {i}: {line}")
            else:
                print("📋 No test output captured")

        return profiling_data

    def _parse_profiling_output(self, output: str) -> List[ProfilingData]:
        """Parse JSON profiling data from test output."""
        profiling_data = []

        for line in output.split("\n"):
            line = line.strip()
            if line.startswith('{"type":"profiling"'):
                try:
                    data = json.loads(line)
                    if data.get("type") == "profiling":
                        operation = data["operation"]
                        tx_size = self.tx_analyzer.get_transaction_size(operation)
                        profiling_data.append(
                            ProfilingData(
                                operation=operation,
                                cu_consumed=data["cu_consumed"],
                                transaction_size=tx_size,
                            )
                        )
                except json.JSONDecodeError:
                    print(f"⚠️  Warning: Failed to parse JSON line: {line}")
                    continue

        return profiling_data


# =============================================================================
# STATISTICAL ANALYSIS
# =============================================================================


class StatisticalAnalyzer:
    """Handles statistical analysis of profiling data."""

    def analyze_operations(
        self, profiling_data: List[ProfilingData]
    ) -> Dict[str, OperationStats]:
        """Analyze profiling data and generate statistics per operation."""
        operations = defaultdict(list)

        # Group by operation type
        tx_operations = defaultdict(list)
        for data in profiling_data:
            operations[data.operation].append(data.cu_consumed)
            tx_operations[data.operation].append(data.transaction_size)

        # Calculate statistics for each operation
        operation_stats = {}
        for operation, cu_values in operations.items():
            tx_values = tx_operations[operation]
            stats = OperationStats(
                operation=operation,
                total_calls=len(cu_values),
                total_cu=sum(cu_values),
                mean_cu=sum(cu_values) / len(cu_values),
                min_cu=min(cu_values),
                max_cu=max(cu_values),
                total_tx_size=sum(tx_values),
                mean_tx_size=sum(tx_values) / len(tx_values),
                min_tx_size=min(tx_values),
                max_tx_size=max(tx_values),
            )
            operation_stats[operation] = stats

        return operation_stats


# =============================================================================
# REPORT GENERATION
# =============================================================================


class ReportGenerator:
    """Generates markdown reports from profiling analysis."""

    def generate_markdown_report(
        self, operation_stats: Dict[str, OperationStats], output_file: str
    ) -> None:
        """Generate comprehensive markdown report."""
        with open(output_file, "w") as f:
            self._write_header(f, operation_stats)
            self._write_executive_summary(f, operation_stats)
            self._write_detailed_breakdown(f, operation_stats)

    def _write_header(self, f, operation_stats: Dict[str, OperationStats]) -> None:
        """Write report header."""
        f.write("# Contra Escrow Program - CU Analysis Report\n\n")
        f.write(f"Generated on: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}\n\n")

        total_operations = len(operation_stats)
        total_calls = sum(stats.total_calls for stats in operation_stats.values())
        f.write(f"Total operation types analyzed: {total_operations}\n")
        f.write(f"Total test calls analyzed: {total_calls}\n\n")

    def _write_executive_summary(
        self, f, operation_stats: Dict[str, OperationStats]
    ) -> None:
        """Write executive summary section."""
        f.write("## Executive Summary\n\n")

        if not operation_stats:
            f.write("No profiling data available.\n\n")
            return

        # Find most expensive operations
        sorted_operations = sorted(
            operation_stats.values(), key=lambda x: x.mean_cu, reverse=True
        )

        if sorted_operations:
            most_expensive = sorted_operations[0]
            least_expensive = sorted_operations[-1]

            f.write(
                f"- **Most expensive operation**: `{most_expensive.operation}` ({most_expensive.mean_cu:.0f} CUs average)\n"
            )
            f.write(
                f"- **Least expensive operation**: `{least_expensive.operation}` ({least_expensive.mean_cu:.0f} CUs average)\n"
            )

            # Calculate total CU consumption
            total_cu = sum(stats.total_cu for stats in operation_stats.values())
            f.write(
                f"- **Total CU consumption**: {total_cu:,} CUs across all operations\n"
            )
            # Peak operations
            peak_cu_operation = max(operation_stats.values(), key=lambda x: x.max_cu)
            peak_tx_operation = max(
                operation_stats.values(), key=lambda x: x.max_tx_size
            )
            f.write(
                f"- **Peak single operation**: `{peak_cu_operation.operation}` ({peak_cu_operation.max_cu} CUs)\n"
            )
            f.write(
                f"- **Largest transaction**: `{peak_tx_operation.operation}` ({peak_tx_operation.max_tx_size} bytes)\n\n"
            )

    def _write_detailed_breakdown(
        self, f, operation_stats: Dict[str, OperationStats]
    ) -> None:
        """Write detailed per-operation breakdown."""
        f.write("## Detailed Operation Breakdown\n\n")

        if not operation_stats:
            f.write("No profiling data available.\n\n")
            return

        # Sort by mean CU consumption (descending)
        sorted_operations = sorted(
            operation_stats.values(), key=lambda x: x.mean_cu, reverse=True
        )

        f.write("| Operation | Total Calls | Total CU | TX Size |\n")
        f.write("|-----------|-------------|----------|----------|\n")

        for stats in sorted_operations:
            f.write(
                f"| {stats.operation} | {stats.total_calls} | {stats.total_cu:,} | {stats.max_tx_size} bytes |\n"
            )

        f.write("\n")


# =============================================================================
# MAIN FUNCTION
# =============================================================================


def main():
    """Main entry point for the CU analysis report generator."""
    import argparse

    parser = argparse.ArgumentParser(
        description="Generate CU analysis reports from integration tests"
    )
    parser.add_argument(
        "--output", default=DEFAULT_OUTPUT_FILE, help="Output report file"
    )
    args = parser.parse_args()

    print("🔥 Contra Escrow CU Analysis Report Generator")
    print("=" * 50)

    # Collect profiling data from integration tests
    collector = ProfilingCollector()
    profiling_data = collector.run_integration_tests()

    if not profiling_data:
        print("❌ No profiling data collected")
        sys.exit(1)

    # Analyze the data
    print("📊 Analyzing profiling data...")
    analyzer = StatisticalAnalyzer()
    operation_stats = analyzer.analyze_operations(profiling_data)

    if not operation_stats:
        print("❌ No operation statistics generated")
        sys.exit(1)

    print(f"✅ Generated statistics for {len(operation_stats)} operation types")

    # Generate markdown report
    print(f"📝 Generating markdown report: {args.output}")
    report_generator = ReportGenerator()
    report_generator.generate_markdown_report(operation_stats, args.output)

    print(f"🎉 Report generated successfully: {args.output}")

    # Print summary
    total_calls = sum(stats.total_calls for stats in operation_stats.values())
    print(
        f"🔍 Summary: Analyzed {len(operation_stats)} operations with {total_calls} total calls"
    )


if __name__ == "__main__":
    main()
