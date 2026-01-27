import { expect } from '@jest/globals';
import {
    getOperatorEncoder,
    getOperatorDecoder,
    getOperatorCodec,
    getOperatorSize,
    type Operator,
} from '../../../src/generated';

// Expected size calculation based on program structure
const EXPECTED_SIZE =
    1 + // discriminator
    1; // bump

describe('Operator Account', () => {
    describe('Encoder/Decoder functionality', () => {
        it('should encode and decode operator data correctly', () => {
            const testOperator: Operator = {
                discriminator: 2,
                bump: 250,
            };

            // Test encoding
            const encoder = getOperatorEncoder();
            const encodedData = encoder.encode(testOperator);

            // Test decoding
            const decoder = getOperatorDecoder();
            const decodedOperator = decoder.decode(encodedData);

            // Verify all fields are correctly encoded/decoded
            expect(decodedOperator.discriminator).toBe(testOperator.discriminator);
            expect(decodedOperator.bump).toBe(testOperator.bump);
        });

        it('should handle combined codec correctly', () => {
            const testOperator: Operator = {
                discriminator: 255,
                bump: 127,
            };

            // Test combined codec
            const codec = getOperatorCodec();
            const encodedData = codec.encode(testOperator);
            const decodedOperator = codec.decode(encodedData);

            // Verify round-trip encoding/decoding
            expect(decodedOperator).toEqual(testOperator);
        });

        it('should handle different bump values (u8)', () => {
            const testBumps = [0, 1, 127, 250, 254, 255];

            for (const bump of testBumps) {
                const testOperator: Operator = {
                    discriminator: 2,
                    bump,
                };

                const codec = getOperatorCodec();
                const encodedData = codec.encode(testOperator);
                const decodedOperator = codec.decode(encodedData);

                expect(decodedOperator.bump).toBe(bump);
                expect(typeof decodedOperator.bump).toBe('number');
            }
        });
    });

    describe('Structure validation', () => {
        it('should validate operator structure fields exist', () => {
            const testOperator: Operator = {
                discriminator: 2,
                bump: 250,
            };

            // Verify all required fields are present
            expect(testOperator).toHaveProperty('discriminator');
            expect(testOperator).toHaveProperty('bump');
        });

        it('should validate operator structure field types', () => {
            const testOperator: Operator = {
                discriminator: 2,
                bump: 250,
            };

            // Verify field types
            expect(typeof testOperator.discriminator).toBe('number');
            expect(typeof testOperator.bump).toBe('number');
        });
    });

    describe('Size validation', () => {
        it('should report correct account size (2 bytes)', () => {
            const accountSize = getOperatorSize();
            expect(accountSize).toBe(EXPECTED_SIZE);
        });

        it('should validate encoded data matches expected size', () => {
            const testOperator: Operator = {
                discriminator: 2,
                bump: 250,
            };

            const encoder = getOperatorEncoder();
            const encodedData = encoder.encode(testOperator);
            const reportedSize = getOperatorSize();
            const actualSize = encodedData.length;

            expect(encodedData).toHaveLength(EXPECTED_SIZE);
            expect(reportedSize).toBe(EXPECTED_SIZE);
            expect(actualSize).toBe(EXPECTED_SIZE);
        });

        it('should validate size consistency across multiple operators', () => {
            const testOperators: Operator[] = [
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

            const encoder = getOperatorEncoder();

            for (const operator of testOperators) {
                const encodedData = encoder.encode(operator);
                expect(encodedData).toHaveLength(EXPECTED_SIZE);
            }
        });
    });

    describe('Edge case validation', () => {
        it('should handle minimum values', () => {
            const testOperator: Operator = {
                discriminator: 0,
                bump: 0,
            };

            const codec = getOperatorCodec();
            const encodedData = codec.encode(testOperator);
            const decodedOperator = codec.decode(encodedData);

            expect(decodedOperator).toEqual(testOperator);
        });

        it('should handle maximum values', () => {
            const testOperator: Operator = {
                discriminator: 255,
                bump: 255,
            };

            const codec = getOperatorCodec();
            const encodedData = codec.encode(testOperator);
            const decodedOperator = codec.decode(encodedData);

            expect(decodedOperator).toEqual(testOperator);
        });
    });
});
