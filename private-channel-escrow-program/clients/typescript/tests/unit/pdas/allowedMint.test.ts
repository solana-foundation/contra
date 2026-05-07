import { expect } from '@jest/globals';
import { address, assertIsAddress } from '@solana/kit';
import { findAllowedMintPda, type AllowedMintSeeds } from '../../../src/generated/pdas/allowedMint';
import { expectedAllowedMintPda } from './pda-helpers';

describe('AllowedMint PDA', () => {
    // Sample addresses for testing
    const sampleInstance1 = address('7JttKuoVeFqzMkspfBvTxGiHjYr9dT4GozFvJC5A7Nfa');
    const sampleInstance2 = address('BMLRKsFtgM7tNPnSS1DMBjJWjqChaY5AKHkNNHBooqpE');
    const sampleMint1 = address('So11111111111111111111111111111111111111112'); // Wrapped SOL
    const sampleMint2 = address('EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v'); // USDC

    it('should generate allowedMint PDA matching expected values', async () => {
        const seeds: AllowedMintSeeds = {
            instance: sampleInstance1,
            mint: sampleMint1,
        };

        const generatedPda = await findAllowedMintPda(seeds);
        const expectedPda = await expectedAllowedMintPda(sampleInstance1, sampleMint1);

        expect(generatedPda[0]).toBe(expectedPda[0]); // address
        expect(generatedPda[1]).toBe(expectedPda[1]); // bump
        expect(Array.isArray(generatedPda)).toBe(true);
        expect(generatedPda).toHaveLength(2);
        expect(typeof generatedPda[0]).toBe('string'); // address
        expect(typeof generatedPda[1]).toBe('number'); // bump
        expect(generatedPda[1]).toBeGreaterThanOrEqual(0);
        expect(generatedPda[1]).toBeLessThanOrEqual(255);
        assertIsAddress(generatedPda[0]);
    });

    it('should generate different PDAs for different mints', async () => {
        const seeds1: AllowedMintSeeds = {
            instance: sampleInstance1,
            mint: sampleMint1,
        };
        const seeds2: AllowedMintSeeds = {
            instance: sampleInstance1,
            mint: sampleMint2,
        };

        const pda1 = await findAllowedMintPda(seeds1);
        const pda2 = await findAllowedMintPda(seeds2);

        expect(pda1[0]).not.toBe(pda2[0]); // addresses should be different
    });

    it('should generate different PDAs for different instances', async () => {
        const seeds1: AllowedMintSeeds = {
            instance: sampleInstance1,
            mint: sampleMint1,
        };
        const seeds2: AllowedMintSeeds = {
            instance: sampleInstance2,
            mint: sampleMint1,
        };

        const pda1 = await findAllowedMintPda(seeds1);
        const pda2 = await findAllowedMintPda(seeds2);

        expect(pda1[0]).not.toBe(pda2[0]); // addresses should be different
    });

    it('should generate unique PDAs for each instance-mint combination', async () => {
        const combinations = [
            { instance: sampleInstance1, mint: sampleMint1 },
            { instance: sampleInstance1, mint: sampleMint2 },
            { instance: sampleInstance2, mint: sampleMint1 },
            { instance: sampleInstance2, mint: sampleMint2 },
        ];

        const pdas = await Promise.all(combinations.map(seeds => findAllowedMintPda(seeds)));

        const addresses = pdas.map(pda => pda[0]);
        const uniqueAddresses = new Set(addresses);

        // All PDAs should be unique
        expect(uniqueAddresses.size).toBe(combinations.length);
    });

    it('should use custom program address when provided', async () => {
        const customProgramId = address('11111111111111111111111111111111');
        const seeds: AllowedMintSeeds = {
            instance: sampleInstance1,
            mint: sampleMint1,
        };

        const defaultPda = await findAllowedMintPda(seeds);
        const customPda = await findAllowedMintPda(seeds, { programAddress: customProgramId });

        expect(defaultPda[0]).not.toBe(customPda[0]); // addresses should be different
    });

    it('should be deterministic for same inputs', async () => {
        const seeds: AllowedMintSeeds = {
            instance: sampleInstance1,
            mint: sampleMint1,
        };

        const pda1 = await findAllowedMintPda(seeds);
        const pda2 = await findAllowedMintPda(seeds);

        expect(pda1[0]).toBe(pda2[0]); // same address
        expect(pda1[1]).toBe(pda2[1]); // same bump
    });
});
