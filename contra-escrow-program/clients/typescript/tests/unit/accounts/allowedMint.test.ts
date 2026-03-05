import { expect } from '@jest/globals';
import {
    getAllowedMintEncoder,
    getAllowedMintDecoder,
    getAllowedMintCodec,
    type AllowedMint,
} from '../../../src/generated';

// Expected size calculation based on program structure
const EXPECTED_SIZE =
    1 + // discriminator
    1; // bump

describe('AllowedMint Account', () => {
    describe('Encoder/Decoder functionality', () => {
        it('should encode and decode allowedMint data correctly', () => {
            const testAllowedMint: AllowedMint = {
                discriminator: 1,
                bump: 250,
            };

            // Test encoding
            const encoder = getAllowedMintEncoder();
            const encodedData = encoder.encode(testAllowedMint);

            // Test decoding
            const decoder = getAllowedMintDecoder();
            const decodedAllowedMint = decoder.decode(encodedData);

            // Verify all fields are correctly encoded/decoded
            expect(decodedAllowedMint.discriminator).toBe(testAllowedMint.discriminator);
            expect(decodedAllowedMint.bump).toBe(testAllowedMint.bump);
        });

        it('should handle combined codec correctly', () => {
            const testAllowedMint: AllowedMint = {
                discriminator: 255,
                bump: 127,
            };

            // Test combined codec
            const codec = getAllowedMintCodec();
            const encodedData = codec.encode(testAllowedMint);
            const decodedAllowedMint = codec.decode(encodedData);

            // Verify round-trip encoding/decoding
            expect(decodedAllowedMint).toEqual(testAllowedMint);
        });

        it('should handle different bump values (u8)', () => {
            const testBumps = [0, 1, 127, 250, 254, 255];

            for (const bump of testBumps) {
                const testAllowedMint: AllowedMint = {
                    discriminator: 1,
                    bump,
                };

                const codec = getAllowedMintCodec();
                const encodedData = codec.encode(testAllowedMint);
                const decodedAllowedMint = codec.decode(encodedData);

                expect(decodedAllowedMint.bump).toBe(bump);
                expect(typeof decodedAllowedMint.bump).toBe('number');
            }
        });
    });

    describe('Structure validation', () => {
        it('should validate allowedMint structure fields exist', () => {
            const testAllowedMint: AllowedMint = {
                discriminator: 1,
                bump: 250,
            };

            // Verify all required fields are present
            expect(testAllowedMint).toHaveProperty('discriminator');
            expect(testAllowedMint).toHaveProperty('bump');
        });

        it('should validate allowedMint structure field types', () => {
            const testAllowedMint: AllowedMint = {
                discriminator: 1,
                bump: 250,
            };

            // Verify field types
            expect(typeof testAllowedMint.discriminator).toBe('number');
            expect(typeof testAllowedMint.bump).toBe('number');
        });
    });

    describe('Size validation', () => {
        it('should report correct account size (2 bytes)', () => {
            const accountSize = getAllowedMintEncoder().fixedSize;
            expect(accountSize).toBe(EXPECTED_SIZE);
        });

        it('should validate encoded data matches expected size', () => {
            const testAllowedMint: AllowedMint = {
                discriminator: 1,
                bump: 250,
            };

            const encoder = getAllowedMintEncoder();
            const encodedData = encoder.encode(testAllowedMint);
            const reportedSize = getAllowedMintEncoder().fixedSize;
            const actualSize = encodedData.length;

            expect(encodedData).toHaveLength(EXPECTED_SIZE);
            expect(reportedSize).toBe(EXPECTED_SIZE);
            expect(actualSize).toBe(EXPECTED_SIZE);
        });

        it('should validate size consistency across multiple allowedMints', () => {
            const testAllowedMints: AllowedMint[] = [
                {
                    discriminator: 0,
                    bump: 100,
                },
                {
                    discriminator: 255,
                    bump: 255,
                },
                {
                    discriminator: 127,
                    bump: 50,
                },
            ];

            const encoder = getAllowedMintEncoder();

            for (const allowedMint of testAllowedMints) {
                const encodedData = encoder.encode(allowedMint);
                expect(encodedData).toHaveLength(EXPECTED_SIZE);
            }
        });
    });

    describe('Edge case validation', () => {
        it('should handle minimum values', () => {
            const testAllowedMint: AllowedMint = {
                discriminator: 0,
                bump: 0,
            };

            const codec = getAllowedMintCodec();
            const encodedData = codec.encode(testAllowedMint);
            const decodedAllowedMint = codec.decode(encodedData);

            expect(decodedAllowedMint).toEqual(testAllowedMint);
        });

        it('should handle maximum values', () => {
            const testAllowedMint: AllowedMint = {
                discriminator: 255,
                bump: 255,
            };

            const codec = getAllowedMintCodec();
            const encodedData = codec.encode(testAllowedMint);
            const decodedAllowedMint = codec.decode(encodedData);

            expect(decodedAllowedMint).toEqual(testAllowedMint);
        });
    });
});
