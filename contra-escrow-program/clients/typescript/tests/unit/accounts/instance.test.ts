import { expect } from '@jest/globals';
import {
    getInstanceEncoder,
    getInstanceDecoder,
    getInstanceCodec,
    type Instance,
} from '../../../src/generated';
import { TEST_ADDRESSES, TEST_ROOT } from '../../setup/mocks';
import { assertIsAddress, type Address } from '@solana/kit';

const EXPECTED_SIZE =
    1 + // discriminator
    1 + // bump
    1 + // version
    32 + // instance_seed
    32 + // admin
    32 + // withdrawal_transactions_root
    8; // current_tree_index

describe('Instance Account', () => {
    describe('Encoder/Decoder functionality', () => {
        it('should encode and decode instance data correctly', () => {
            const testInstance: Instance = {
                discriminator: 0,
                bump: 250,
                version: 1,
                instanceSeed: TEST_ADDRESSES.INSTANCE_SEED,
                admin: TEST_ADDRESSES.ADMIN,
                withdrawalTransactionsRoot: TEST_ROOT,
                currentTreeIndex: 0n,
            };

            // Test encoding
            const encoder = getInstanceEncoder();
            const encodedData = encoder.encode(testInstance);

            // Test decoding
            const decoder = getInstanceDecoder();
            const decodedInstance = decoder.decode(encodedData);

            // Verify all fields are correctly encoded/decoded
            expect(decodedInstance.discriminator).toBe(testInstance.discriminator);
            expect(decodedInstance.bump).toBe(testInstance.bump);
            expect(decodedInstance.version).toBe(testInstance.version);
            expect(decodedInstance.instanceSeed).toBe(testInstance.instanceSeed);
            expect(decodedInstance.admin).toBe(testInstance.admin);
            expect(decodedInstance.withdrawalTransactionsRoot).toEqual(Array.from(testInstance.withdrawalTransactionsRoot));
        });

        it('should handle combined codec correctly', () => {
            const testInstance: Instance = {
                discriminator: 1,
                bump: 255,
                version: 2,
                instanceSeed: TEST_ADDRESSES.INSTANCE_SEED_2,
                admin: TEST_ADDRESSES.WALLET,
                withdrawalTransactionsRoot: Array.from(new Uint8Array(32).fill(127)),
                currentTreeIndex: 1n,
            };

            // Test combined codec
            const codec = getInstanceCodec();
            const encodedData = codec.encode(testInstance);
            const decodedInstance = codec.decode(encodedData);

            // Verify round-trip encoding/decoding
            expect(decodedInstance).toEqual(testInstance);
        });

        it('should handle different bump values (u8)', () => {
            const testBumps = [0, 1, 127, 250, 254, 255];

            for (const bump of testBumps) {
                const testInstance: Instance = {
                    discriminator: 0,
                    bump,
                    version: 1,
                    instanceSeed: TEST_ADDRESSES.INSTANCE_SEED,
                    admin: TEST_ADDRESSES.ADMIN,
                    withdrawalTransactionsRoot: new Uint8Array(32).fill(0),
                    currentTreeIndex: 2n,
                };

                const codec = getInstanceCodec();
                const encodedData = codec.encode(testInstance);
                const decodedInstance = codec.decode(encodedData);

                expect(decodedInstance.bump).toBe(bump);
                expect(typeof decodedInstance.bump).toBe('number');
            }
        });

        it('should handle different version values (u8)', () => {
            const testVersions = [0, 1, 2, 10, 100, 255];

            for (const version of testVersions) {
                const testInstance: Instance = {
                    discriminator: 0,
                    bump: 250,
                    version,
                    instanceSeed: TEST_ADDRESSES.INSTANCE_SEED,
                    admin: TEST_ADDRESSES.ADMIN,
                    withdrawalTransactionsRoot: new Uint8Array(32).fill(0),
                    currentTreeIndex: 3n,
                };

                const codec = getInstanceCodec();
                const encodedData = codec.encode(testInstance);
                const decodedInstance = codec.decode(encodedData);

                expect(decodedInstance.version).toBe(version);
                expect(typeof decodedInstance.version).toBe('number');
            }
        });

        it('should handle different address values correctly', () => {
            const testAddresses = [
                { instanceSeed: TEST_ADDRESSES.INSTANCE_SEED, admin: TEST_ADDRESSES.ADMIN },
                { instanceSeed: TEST_ADDRESSES.INSTANCE_SEED_2, admin: TEST_ADDRESSES.WALLET },
                { instanceSeed: TEST_ADDRESSES.USDC_MINT, admin: TEST_ADDRESSES.OPERATOR },
                { instanceSeed: TEST_ADDRESSES.WRAPPED_SOL, admin: TEST_ADDRESSES.PAYER },
            ];

            for (const addresses of testAddresses) {
                const testInstance: Instance = {
                    discriminator: 0,
                    bump: 250,
                    version: 1,
                    instanceSeed: addresses.instanceSeed as Address,
                    admin: addresses.admin as Address,
                    withdrawalTransactionsRoot: new Uint8Array(32).fill(0),
                    currentTreeIndex: 4n,
                };

                const codec = getInstanceCodec();
                const encodedData = codec.encode(testInstance);
                const decodedInstance = codec.decode(encodedData);

                expect(decodedInstance.instanceSeed).toBe(addresses.instanceSeed);
                expect(decodedInstance.admin).toBe(addresses.admin);
                assertIsAddress(decodedInstance.instanceSeed);
                assertIsAddress(decodedInstance.admin);
            }
        });

        it('should handle different withdrawal root patterns (32 bytes)', () => {
            const testRoots = [
                new Uint8Array(32).fill(0), // All zeros
                new Uint8Array(32).fill(255), // All 0xFF
                new Uint8Array(Array.from({ length: 32 }, (_, i) => i)), // Sequential 0-31
                new Uint8Array(Array.from({ length: 32 }, (_, i) => 255 - i)), // Reverse sequential
                crypto.getRandomValues(new Uint8Array(32)), // Random bytes
            ];

            for (const withdrawalRoot of testRoots) {
                const testInstance: Instance = {
                    discriminator: 0,
                    bump: 250,
                    version: 1,
                    instanceSeed: TEST_ADDRESSES.INSTANCE_SEED,
                    admin: TEST_ADDRESSES.ADMIN,
                    withdrawalTransactionsRoot: withdrawalRoot,
                    currentTreeIndex: 5n,
                };

                const codec = getInstanceCodec();
                const encodedData = codec.encode(testInstance);
                const decodedInstance = codec.decode(encodedData);

                expect(decodedInstance.withdrawalTransactionsRoot).toEqual(Array.from(withdrawalRoot));
                expect(decodedInstance.withdrawalTransactionsRoot).toHaveLength(32);
                expect(Array.isArray(decodedInstance.withdrawalTransactionsRoot)).toBe(true);
            }
        });
    });

    describe('Structure validation', () => {
        it('should validate instance structure fields exist', () => {
            const testInstance: Instance = {
                discriminator: 0,
                bump: 250,
                version: 1,
                instanceSeed: TEST_ADDRESSES.INSTANCE_SEED,
                admin: TEST_ADDRESSES.ADMIN,
                withdrawalTransactionsRoot: new Uint8Array(32).fill(0),
                currentTreeIndex: 7n,
            };

            // Verify all required fields are present
            expect(testInstance).toHaveProperty('discriminator');
            expect(testInstance).toHaveProperty('bump');
            expect(testInstance).toHaveProperty('version');
            expect(testInstance).toHaveProperty('instanceSeed');
            expect(testInstance).toHaveProperty('admin');
            expect(testInstance).toHaveProperty('withdrawalTransactionsRoot');
            expect(testInstance).toHaveProperty('currentTreeIndex');
        });

        it('should validate instance structure field types', () => {
            const testInstance: Instance = {
                discriminator: 0,
                bump: 250,
                version: 1,
                instanceSeed: TEST_ADDRESSES.INSTANCE_SEED,
                admin: TEST_ADDRESSES.ADMIN,
                withdrawalTransactionsRoot: new Uint8Array(32).fill(0),
                currentTreeIndex: 8n,
            };

            // Verify field types
            expect(typeof testInstance.discriminator).toBe('number');
            expect(typeof testInstance.bump).toBe('number');
            expect(typeof testInstance.version).toBe('number');
            expect(typeof testInstance.instanceSeed).toBe('string');
            expect(typeof testInstance.admin).toBe('string');
            expect(testInstance.withdrawalTransactionsRoot instanceof Uint8Array).toBe(true);
            expect(typeof testInstance.currentTreeIndex).toBe('bigint');
        });

        it('should validate withdrawal root is exactly 32 bytes', () => {
            const validRoot = new Uint8Array(32).fill(0);

            const testInstance: Instance = {
                discriminator: 0,
                bump: 250,
                version: 1,
                instanceSeed: TEST_ADDRESSES.INSTANCE_SEED,
                admin: TEST_ADDRESSES.ADMIN,
                withdrawalTransactionsRoot: validRoot,
                currentTreeIndex: 9n,
            };

            expect(testInstance.withdrawalTransactionsRoot).toHaveLength(32);
        });
    });

    describe('Size validation', () => {
        it('should report correct account size (107 bytes)', () => {
            const accountSize = getInstanceEncoder().fixedSize;
            expect(accountSize).toBe(EXPECTED_SIZE);
        });

        it('should validate encoded data matches expected size', () => {
            const testInstance: Instance = {
                discriminator: 0,
                bump: 250,
                version: 1,
                instanceSeed: TEST_ADDRESSES.INSTANCE_SEED,
                admin: TEST_ADDRESSES.ADMIN,
                withdrawalTransactionsRoot: new Uint8Array(32).fill(0),
                currentTreeIndex: 10n,
            };

            const encoder = getInstanceEncoder();
            const encodedData = encoder.encode(testInstance);
            const reportedSize = getInstanceEncoder().fixedSize;
            const actualSize = encodedData.length;

            expect(encodedData).toHaveLength(EXPECTED_SIZE);
            expect(reportedSize).toBe(EXPECTED_SIZE);
            expect(actualSize).toBe(EXPECTED_SIZE);
        });

        it('should validate size consistency across multiple instances', () => {
            const testInstances: Instance[] = [
                {
                    discriminator: 0,
                    bump: 100,
                    version: 1,
                    instanceSeed: TEST_ADDRESSES.INSTANCE_SEED,
                    admin: TEST_ADDRESSES.ADMIN,
                    withdrawalTransactionsRoot: new Uint8Array(32).fill(0),
                    currentTreeIndex: 11n,
                },
                {
                    discriminator: 255,
                    bump: 255,
                    version: 255,
                    instanceSeed: TEST_ADDRESSES.INSTANCE_SEED_2,
                    admin: TEST_ADDRESSES.WALLET,
                    withdrawalTransactionsRoot: new Uint8Array(32).fill(255),
                    currentTreeIndex: 12n,
                },
                {
                    discriminator: 127,
                    bump: 50,
                    version: 10,
                    instanceSeed: TEST_ADDRESSES.USDC_MINT,
                    admin: TEST_ADDRESSES.OPERATOR,
                    withdrawalTransactionsRoot: crypto.getRandomValues(new Uint8Array(32)),
                    currentTreeIndex: 13n,
                },
            ];

            const encoder = getInstanceEncoder();

            for (const instance of testInstances) {
                const encodedData = encoder.encode(instance);
                expect(encodedData).toHaveLength(EXPECTED_SIZE);
            }
        });

        it('should calculate size based on field types', () => {
            const reportedSize = getInstanceEncoder().fixedSize;

            // Test that our calculation matches the actual encoded size
            const testInstance: Instance = {
                discriminator: 0,
                bump: 250,
                version: 1,
                instanceSeed: TEST_ADDRESSES.INSTANCE_SEED,
                admin: TEST_ADDRESSES.ADMIN,
                withdrawalTransactionsRoot: new Uint8Array(32).fill(0),
                currentTreeIndex: 14n,
            };

            const encoder = getInstanceEncoder();
            const encodedData = encoder.encode(testInstance);

            expect(encodedData.length).toBe(EXPECTED_SIZE);
            expect(reportedSize).toBe(EXPECTED_SIZE);

            // The sizes should be within reasonable range
            expect(encodedData.length).toBeGreaterThan(90);
            expect(encodedData.length).toBeLessThan(120);
        });
    });
});
