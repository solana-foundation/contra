import { expect } from '@jest/globals';
import {
    getSetNewAdminInstruction,
    getSetNewAdminInstructionDataCodec,
    SET_NEW_ADMIN_DISCRIMINATOR,
    PRIVATE_CHANNEL_ESCROW_PROGRAM_PROGRAM_ADDRESS,
} from '../../../src/generated';
import { mockTransactionSigner, TEST_ADDRESSES, EXPECTED_PROGRAM_ADDRESS } from '../../setup/mocks';
import { AccountRole } from '@solana/kit';

describe('setNewAdmin', () => {
    describe('Instruction data validation', () => {
        it('should encode instruction data with correct discriminator (5)', () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const currentAdmin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);
            const newAdmin = mockTransactionSigner(TEST_ADDRESSES.WALLET);

            const instruction = getSetNewAdminInstruction({
                payer,
                currentAdmin,
                instance: TEST_ADDRESSES.INSTANCE,
                newAdmin,
            });

            const decodedData = getSetNewAdminInstructionDataCodec().decode(instruction.data);

            // Verify discriminator is 5 as defined in the program
            expect(decodedData.discriminator).toBe(SET_NEW_ADMIN_DISCRIMINATOR);
            expect(decodedData.discriminator).toBe(5);
        });

        it('should have no additional parameters beyond discriminator', () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const currentAdmin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);
            const newAdmin = mockTransactionSigner(TEST_ADDRESSES.WALLET);

            const instruction = getSetNewAdminInstruction({
                payer,
                currentAdmin,
                instance: TEST_ADDRESSES.INSTANCE,
                newAdmin,
            });

            const decodedData = getSetNewAdminInstructionDataCodec().decode(instruction.data);

            // SetNewAdmin instruction should only have discriminator field
            expect(Object.keys(decodedData)).toEqual(['discriminator']);
            expect(typeof decodedData.discriminator).toBe('number');
        });

        it('should decode instruction data correctly', () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const currentAdmin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);
            const newAdmin = mockTransactionSigner(TEST_ADDRESSES.WALLET);

            const instruction = getSetNewAdminInstruction({
                payer,
                currentAdmin,
                instance: TEST_ADDRESSES.INSTANCE,
                newAdmin,
            });

            // Decode the instruction data
            const decodedData = getSetNewAdminInstructionDataCodec().decode(instruction.data);

            // Verify fields are decoded correctly
            expect(decodedData.discriminator).toBe(SET_NEW_ADMIN_DISCRIMINATOR);
            expect(typeof decodedData.discriminator).toBe('number');

            // Re-encode and verify it matches
            const reEncodedData = getSetNewAdminInstructionDataCodec().encode({});
            expect(reEncodedData).toEqual(instruction.data);
        });
    });

    describe('Account requirements', () => {
        it('should include all required accounts: payer, currentAdmin, instance, newAdmin, eventAuthority, privateChannelEscrowProgram', () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const currentAdmin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);
            const newAdmin = mockTransactionSigner(TEST_ADDRESSES.WALLET);

            const instruction = getSetNewAdminInstruction({
                payer,
                currentAdmin,
                instance: TEST_ADDRESSES.INSTANCE,
                newAdmin,
            });

            // SetNewAdmin should have 6 accounts based on program instruction definition
            expect(instruction.accounts).toHaveLength(6);

            // Account 0: payer (WritableSigner)
            const payerAccount = instruction.accounts[0];
            expect(payerAccount.address).toBe(TEST_ADDRESSES.PAYER);

            // Account 1: currentAdmin (ReadonlySigner)
            const currentAdminAccount = instruction.accounts[1];
            expect(currentAdminAccount.address).toBe(TEST_ADDRESSES.ADMIN);

            // Account 2: instance (Writable)
            const instanceAccount = instruction.accounts[2];
            expect(instanceAccount.address).toBe(TEST_ADDRESSES.INSTANCE);

            // Account 3: newAdmin (ReadonlySigner)
            const newAdminAccount = instruction.accounts[3];
            expect(newAdminAccount.address).toBe(TEST_ADDRESSES.WALLET);

            // Account 4: eventAuthority (Readonly PDA)
            const eventAuthorityAccount = instruction.accounts[4];
            expect(eventAuthorityAccount.address).toBeDefined();

            // Account 5: privateChannelEscrowProgram (Readonly)
            const privateChannelEscrowProgramAccount = instruction.accounts[5];
            expect(privateChannelEscrowProgramAccount.address).toBe(PRIVATE_CHANNEL_ESCROW_PROGRAM_PROGRAM_ADDRESS);
        });

        it('should set correct account permissions (writable/readable/signer)', () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const currentAdmin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);
            const newAdmin = mockTransactionSigner(TEST_ADDRESSES.WALLET);

            const instruction = getSetNewAdminInstruction({
                payer,
                currentAdmin,
                instance: TEST_ADDRESSES.INSTANCE,
                newAdmin,
            });

            // Account 0: payer - should be WritableSigner
            const payerAccount = instruction.accounts[0];
            expect(payerAccount.role).toBe(AccountRole.WRITABLE_SIGNER);

            // Account 1: currentAdmin - should be ReadonlySigner
            const currentAdminAccount = instruction.accounts[1];
            expect(currentAdminAccount.role).toBe(AccountRole.READONLY_SIGNER);

            // Account 2: instance - should be Writable (PDA, not a signer)
            const instanceAccount = instruction.accounts[2];
            expect(instanceAccount.role).toBe(AccountRole.WRITABLE);

            // Account 3: newAdmin - should be ReadonlySigner
            const newAdminAccount = instruction.accounts[3];
            expect(newAdminAccount.role).toBe(AccountRole.READONLY_SIGNER);

            // Account 4: eventAuthority - should be Readonly (PDA, not a signer)
            const eventAuthorityAccount = instruction.accounts[4];
            expect(eventAuthorityAccount.role).toBe(AccountRole.READONLY);

            // Account 5: privateChannelEscrowProgram - should be Readonly
            const privateChannelEscrowProgramAccount = instruction.accounts[5];
            expect(privateChannelEscrowProgramAccount.role).toBe(AccountRole.READONLY);
        });

        it('should use correct program address', () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const currentAdmin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);
            const newAdmin = mockTransactionSigner(TEST_ADDRESSES.WALLET);

            const instruction = getSetNewAdminInstruction({
                payer,
                currentAdmin,
                instance: TEST_ADDRESSES.INSTANCE,
                newAdmin,
            });

            // Verify the instruction uses the correct program address
            expect(instruction.programAddress).toBe(PRIVATE_CHANNEL_ESCROW_PROGRAM_PROGRAM_ADDRESS);
            expect(instruction.programAddress).toBe(EXPECTED_PROGRAM_ADDRESS);

            // Verify privateChannelEscrowProgram uses the correct address
            const privateChannelEscrowProgramAccount = instruction.accounts[5];
            expect(privateChannelEscrowProgramAccount.address).toBe(PRIVATE_CHANNEL_ESCROW_PROGRAM_PROGRAM_ADDRESS);
        });
    });
});
