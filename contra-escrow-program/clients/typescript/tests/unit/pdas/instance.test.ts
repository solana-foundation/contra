import { expect } from '@jest/globals';
import { address, assertIsAddress } from '@solana/kit';
import { findInstancePda, type InstanceSeeds } from '../../../src/generated/pdas/instance';
import { expectedInstancePda } from './pda-helpers';
import { TEST_ADDRESSES } from '../../setup/mocks';

describe('Instance PDA', () => {
    const sampleInstanceSeed1 = TEST_ADDRESSES.INSTANCE_SEED;
    const sampleInstanceSeed2 = address('8cPFGPZbUE7DQPrw24GgTYNkvr2FLnHfgqgjCxEn73K6');

    it('should generate instance PDA matching expected values', async () => {
        const seeds: InstanceSeeds = {
            instanceSeed: sampleInstanceSeed1,
        };

        const generatedPda = await findInstancePda(seeds);
        const expectedPda = await expectedInstancePda(sampleInstanceSeed1);

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

    it('should generate different instance PDAs for different instance seeds', async () => {
        const seeds1: InstanceSeeds = {
            instanceSeed: sampleInstanceSeed1,
        };
        const seeds2: InstanceSeeds = {
            instanceSeed: sampleInstanceSeed2,
        };

        const pda1 = await findInstancePda(seeds1);
        const pda2 = await findInstancePda(seeds2);

        expect(pda1[0]).not.toBe(pda2[0]); // address should be different
    });

    it('should use custom program address when provided', async () => {
        const customProgramId = address('11111111111111111111111111111111');
        const seeds: InstanceSeeds = {
            instanceSeed: sampleInstanceSeed1,
        };

        const defaultPda = await findInstancePda(seeds);
        const customPda = await findInstancePda(seeds, { programAddress: customProgramId });

        expect(defaultPda[0]).not.toBe(customPda[0]); // address should be different
    });

    it('should be deterministic for same inputs', async () => {
        const seeds: InstanceSeeds = {
            instanceSeed: sampleInstanceSeed1,
        };

        const pda1 = await findInstancePda(seeds);
        const pda2 = await findInstancePda(seeds);

        expect(pda1[0]).toBe(pda2[0]); // same address
        expect(pda1[1]).toBe(pda2[1]); // same bump
    });
});
