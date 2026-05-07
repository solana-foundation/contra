import { expect } from '@jest/globals';
import { address, assertIsAddress } from '@solana/kit';
import { findOperatorPda, type OperatorSeeds } from '../../../src/generated/pdas/operator';
import { expectedOperatorPda } from './pda-helpers';

describe('Operator PDA', () => {
    // Sample addresses for testing
    const sampleInstance1 = address('7JttKuoVeFqzMkspfBvTxGiHjYr9dT4GozFvJC5A7Nfa');
    const sampleInstance2 = address('BMLRKsFtgM7tNPnSS1DMBjJWjqChaY5AKHkNNHBooqpE');
    const sampleWallet1 = address('HJKATa5s6jwQzM23DaBJPRJ8qdH7YN7wLPgw9w3gicZD');
    const sampleWallet2 = address('9FYsKrNuEweb55Wa2jaj8wTKYDBvuCG3huhakEj96iN9');

    it('should generate operator PDA matching expected values', async () => {
        const seeds: OperatorSeeds = {
            instance: sampleInstance1,
            wallet: sampleWallet1,
        };

        const generatedPda = await findOperatorPda(seeds);
        const expectedPda = await expectedOperatorPda(sampleInstance1, sampleWallet1);

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

    it('should generate different PDAs for different wallets', async () => {
        const seeds1: OperatorSeeds = {
            instance: sampleInstance1,
            wallet: sampleWallet1,
        };
        const seeds2: OperatorSeeds = {
            instance: sampleInstance1,
            wallet: sampleWallet2,
        };

        const pda1 = await findOperatorPda(seeds1);
        const pda2 = await findOperatorPda(seeds2);

        expect(pda1[0]).not.toBe(pda2[0]); // addresses should be different
    });

    it('should generate different PDAs for different instances', async () => {
        const seeds1: OperatorSeeds = {
            instance: sampleInstance1,
            wallet: sampleWallet1,
        };
        const seeds2: OperatorSeeds = {
            instance: sampleInstance2,
            wallet: sampleWallet1,
        };

        const pda1 = await findOperatorPda(seeds1);
        const pda2 = await findOperatorPda(seeds2);

        expect(pda1[0]).not.toBe(pda2[0]); // addresses should be different
    });

    it('should generate unique PDAs for each instance-wallet combination', async () => {
        const combinations = [
            { instance: sampleInstance1, wallet: sampleWallet1 },
            { instance: sampleInstance1, wallet: sampleWallet2 },
            { instance: sampleInstance2, wallet: sampleWallet1 },
            { instance: sampleInstance2, wallet: sampleWallet2 },
        ];

        const pdas = await Promise.all(combinations.map(seeds => findOperatorPda(seeds)));

        const addresses = pdas.map(pda => pda[0]);
        const uniqueAddresses = new Set(addresses);

        // All PDAs should be unique
        expect(uniqueAddresses.size).toBe(combinations.length);
    });

    it('should use custom program address when provided', async () => {
        const customProgramId = address('11111111111111111111111111111111');
        const seeds: OperatorSeeds = {
            instance: sampleInstance1,
            wallet: sampleWallet1,
        };

        const defaultPda = await findOperatorPda(seeds);
        const customPda = await findOperatorPda(seeds, { programAddress: customProgramId });

        expect(defaultPda[0]).not.toBe(customPda[0]); // addresses should be different
    });

    it('should be deterministic for same inputs', async () => {
        const seeds: OperatorSeeds = {
            instance: sampleInstance1,
            wallet: sampleWallet1,
        };

        const pda1 = await findOperatorPda(seeds);
        const pda2 = await findOperatorPda(seeds);

        expect(pda1[0]).toBe(pda2[0]); // same address
        expect(pda1[1]).toBe(pda2[1]); // same bump
    });
});
