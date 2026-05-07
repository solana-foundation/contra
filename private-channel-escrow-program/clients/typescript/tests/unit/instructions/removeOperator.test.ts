import { expect } from '@jest/globals';
import {
    getRemoveOperatorInstructionAsync,
    getRemoveOperatorInstructionDataCodec,
    REMOVE_OPERATOR_DISCRIMINATOR,
    PRIVATE_CHANNEL_ESCROW_PROGRAM_PROGRAM_ADDRESS,
    findOperatorPda,
} from '../../../src/generated';
import { mockTransactionSigner, TEST_ADDRESSES, EXPECTED_PROGRAM_ADDRESS } from '../../setup/mocks';
import { AccountRole } from '@solana/kit';
import { SYSTEM_PROGRAM_ADDRESS } from '@solana-program/system';

describe('removeOperator', () => {
    describe('Instruction data validation', () => {
        it('should encode instruction data with correct discriminator (4)', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const admin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);

            const instruction = await getRemoveOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE,
                operator: TEST_ADDRESSES.OPERATOR,
            });

            const decodedData = getRemoveOperatorInstructionDataCodec().decode(instruction.data);

            // Verify discriminator is 4 as defined in the program
            expect(decodedData.discriminator).toBe(REMOVE_OPERATOR_DISCRIMINATOR);
            expect(decodedData.discriminator).toBe(4);
        });

        it('should have no additional parameters beyond discriminator', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const admin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);

            const instruction = await getRemoveOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE,
                operator: TEST_ADDRESSES.OPERATOR,
            });

            const decodedData = getRemoveOperatorInstructionDataCodec().decode(instruction.data);

            // RemoveOperator instruction should only have discriminator field
            expect(Object.keys(decodedData)).toEqual(['discriminator']);
            expect(typeof decodedData.discriminator).toBe('number');
        });

        it('should decode instruction data correctly', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const admin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);

            const instruction = await getRemoveOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE,
                operator: TEST_ADDRESSES.OPERATOR,
            });

            // Decode the instruction data
            const decodedData = getRemoveOperatorInstructionDataCodec().decode(instruction.data);

            // Verify fields are decoded correctly
            expect(decodedData.discriminator).toBe(REMOVE_OPERATOR_DISCRIMINATOR);
            expect(typeof decodedData.discriminator).toBe('number');

            // Re-encode and verify it matches
            const reEncodedData = getRemoveOperatorInstructionDataCodec().encode({});
            expect(reEncodedData).toEqual(instruction.data);
        });
    });

    describe('Account requirements', () => {
        it('should include all required accounts: payer, admin, instance, operator, operatorPda, systemProgram, eventAuthority, privateChannelEscrowProgram', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const admin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);

            const instruction = await getRemoveOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE,
                operator: TEST_ADDRESSES.OPERATOR,
            });

            // RemoveOperator should have 8 accounts based on program instruction definition
            expect(instruction.accounts).toHaveLength(8);

            // Account 0: payer (WritableSigner)
            const payerAccount = instruction.accounts[0];
            expect(payerAccount.address).toBe(TEST_ADDRESSES.PAYER);

            // Account 1: admin (ReadonlySigner)
            const adminAccount = instruction.accounts[1];
            expect(adminAccount.address).toBe(TEST_ADDRESSES.ADMIN);

            // Account 2: instance (Readonly PDA)
            const instanceAccount = instruction.accounts[2];
            expect(instanceAccount.address).toBe(TEST_ADDRESSES.INSTANCE);

            // Account 3: operator (Readonly)
            const operatorAccount = instruction.accounts[3];
            expect(operatorAccount.address).toBe(TEST_ADDRESSES.OPERATOR);

            // Account 4: operatorPda (Writable PDA)
            const operatorPdaAccount = instruction.accounts[4];
            expect(operatorPdaAccount.address).toBeDefined();

            // Account 5: systemProgram (Readonly)
            const systemProgramAccount = instruction.accounts[5];
            expect(systemProgramAccount.address).toBe(SYSTEM_PROGRAM_ADDRESS);

            // Account 6: eventAuthority (Readonly PDA)
            const eventAuthorityAccount = instruction.accounts[6];
            expect(eventAuthorityAccount.address).toBeDefined();

            // Account 7: privateChannelEscrowProgram (Readonly)
            const privateChannelEscrowProgramAccount = instruction.accounts[7];
            expect(privateChannelEscrowProgramAccount.address).toBe(PRIVATE_CHANNEL_ESCROW_PROGRAM_PROGRAM_ADDRESS);
        });

        it('should set correct account permissions (writable/readable/signer)', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const admin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);

            const instruction = await getRemoveOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE,
                operator: TEST_ADDRESSES.OPERATOR,
            });

            // Account 0: payer - should be WritableSigner
            const payerAccount = instruction.accounts[0];
            expect(payerAccount.role).toBe(AccountRole.WRITABLE_SIGNER);

            // Account 1: admin - should be ReadonlySigner
            const adminAccount = instruction.accounts[1];
            expect(adminAccount.role).toBe(AccountRole.READONLY_SIGNER);

            // Account 2: instance - should be Readonly (PDA, not a signer)
            const instanceAccount = instruction.accounts[2];
            expect(instanceAccount.role).toBe(AccountRole.READONLY);

            // Account 3: operator - should be Readonly (not a signer)
            const operatorAccount = instruction.accounts[3];
            expect(operatorAccount.role).toBe(AccountRole.READONLY);

            // Account 4: operatorPda - should be Writable (PDA, not a signer)
            const operatorPdaAccount = instruction.accounts[4];
            expect(operatorPdaAccount.role).toBe(AccountRole.WRITABLE);

            // Account 5: systemProgram - should be Readonly
            const systemProgramAccount = instruction.accounts[5];
            expect(systemProgramAccount.role).toBe(AccountRole.READONLY);

            // Account 6: eventAuthority - should be Readonly (PDA, not a signer)
            const eventAuthorityAccount = instruction.accounts[6];
            expect(eventAuthorityAccount.role).toBe(AccountRole.READONLY);

            // Account 7: privateChannelEscrowProgram - should be Readonly
            const privateChannelEscrowProgramAccount = instruction.accounts[7];
            expect(privateChannelEscrowProgramAccount.role).toBe(AccountRole.READONLY);
        });

        it('should use correct program address', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const admin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);

            const instruction = await getRemoveOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE,
                operator: TEST_ADDRESSES.OPERATOR,
            });

            // Verify the instruction uses the correct program address
            expect(instruction.programAddress).toBe(PRIVATE_CHANNEL_ESCROW_PROGRAM_PROGRAM_ADDRESS);
            expect(instruction.programAddress).toBe(EXPECTED_PROGRAM_ADDRESS);

            // Verify systemProgram uses the correct address
            const systemProgramAccount = instruction.accounts[5];
            expect(systemProgramAccount.address).toBe(SYSTEM_PROGRAM_ADDRESS);

            // Verify privateChannelEscrowProgram uses the correct address
            const privateChannelEscrowProgramAccount = instruction.accounts[7];
            expect(privateChannelEscrowProgramAccount.address).toBe(PRIVATE_CHANNEL_ESCROW_PROGRAM_PROGRAM_ADDRESS);
        });
    });

    describe('Automatic operator PDA derivation', () => {
        it('should automatically derive operator PDA when not provided', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const admin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);

            // Get expected operator PDA using findOperatorPda
            const [expectedOperatorPda] = await findOperatorPda({
                instance: TEST_ADDRESSES.INSTANCE,
                wallet: TEST_ADDRESSES.OPERATOR,
            });

            // Generate instruction without providing operatorPda - should be auto-derived
            const instruction = await getRemoveOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE,
                operator: TEST_ADDRESSES.OPERATOR,
                // Not providing operatorPda - should be auto-derived
            });

            // Verify the automatically derived operator PDA matches expected address
            expect(instruction.accounts[4].address).toBe(expectedOperatorPda);
            const decodedData = getRemoveOperatorInstructionDataCodec().decode(instruction.data);
            expect(decodedData.discriminator).toBe(REMOVE_OPERATOR_DISCRIMINATOR);
        });

        it('should use provided operator PDA when supplied (override auto-derivation)', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const admin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);

            // Use a different instance/wallet combination for override PDA
            const overriddenOperatorPda = await findOperatorPda({
                instance: TEST_ADDRESSES.INSTANCE,
                wallet: TEST_ADDRESSES.WALLET,
            });

            const instruction = await getRemoveOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE,
                operator: TEST_ADDRESSES.OPERATOR,
                operatorPda: overriddenOperatorPda[0],
            });

            // Verify the provided address is used instead of auto-derived one
            expect(instruction.accounts[4].address).toBe(overriddenOperatorPda[0]);
            const decodedData = getRemoveOperatorInstructionDataCodec().decode(instruction.data);
            expect(decodedData.discriminator).toBe(REMOVE_OPERATOR_DISCRIMINATOR);
        });

        it('should derive different PDAs for different instance/wallet combinations', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const admin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);

            // First instruction with INSTANCE + OPERATOR
            const instruction1 = await getRemoveOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE,
                operator: TEST_ADDRESSES.OPERATOR,
            });

            // Second instruction with INSTANCE + WALLET (different operator)
            const instruction2 = await getRemoveOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE,
                operator: TEST_ADDRESSES.WALLET,
            });

            // The operator PDAs should be different because they use different wallet addresses
            expect(instruction1.accounts[4].address).not.toBe(instruction2.accounts[4].address);
        });
    });
});
