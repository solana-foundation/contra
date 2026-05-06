import { expect } from '@jest/globals';
import { address, assertIsAddress } from '@solana/kit';
import { findEventAuthorityPda } from '../../../src/generated/pdas/eventAuthority';
import { expectedEventAuthorityPda } from './pda-helpers';

describe('EventAuthority PDA', () => {
    it('should generate event authority PDA matching expected values', async () => {
        const generatedPda = await findEventAuthorityPda();
        const expectedPda = await expectedEventAuthorityPda();

        expect(generatedPda[0]).toBe(expectedPda[0]); // address
        expect(generatedPda[1]).toBe(expectedPda[1]); // bump
        expect(typeof generatedPda[0]).toBe('string');
        expect(typeof generatedPda[1]).toBe('number');
        expect(generatedPda[1]).toBeGreaterThanOrEqual(0);
        expect(generatedPda[1]).toBeLessThanOrEqual(255);
        assertIsAddress(generatedPda[0]);
    });

    it('should use custom program address when provided', async () => {
        const customProgramId = address('11111111111111111111111111111111');

        const defaultPda = await findEventAuthorityPda();
        const customPda = await findEventAuthorityPda({ programAddress: customProgramId });

        expect(defaultPda[0]).not.toBe(customPda[0]); // addresses should be different
    });

    it('should be deterministic (always return same PDA)', async () => {
        const pda1 = await findEventAuthorityPda();
        const pda2 = await findEventAuthorityPda();

        expect(pda1[0]).toBe(pda2[0]); // same address
        expect(pda1[1]).toBe(pda2[1]); // same bump
    });
});
