import { expect } from '@jest/globals';
import { AccountRole } from '@solana/kit';
import { SYSTEM_PROGRAM_ADDRESS } from '@solana-program/system';
import {
    getAddOperatorInstructionAsync,
    getAddOperatorInstructionDataCodec,
    findOperatorPda,
    ADD_OPERATOR_DISCRIMINATOR,
    PRIVATE_CHANNEL_ESCROW_PROGRAM_PROGRAM_ADDRESS,
} from '../../../src/generated';
import { mockTransactionSigner, TEST_ADDRESSES, EXPECTED_PROGRAM_ADDRESS } from '../../setup/mocks';

describe('addOperator', () => {
    describe('Instruction data validation', () => {
        it('should encode instruction data with correct discriminator (3) and bump parameter', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const admin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);
            const testBump = 42;

            const instruction = await getAddOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE,
                operator: TEST_ADDRESSES.OPERATOR,
                bump: testBump,
            });

            const decodedData = getAddOperatorInstructionDataCodec().decode(instruction.data);

            // Verify discriminator is 3 as defined in the program
            expect(decodedData.discriminator).toBe(ADD_OPERATOR_DISCRIMINATOR);
            expect(decodedData.discriminator).toBe(3);

            // Verify bump is correctly encoded
            expect(decodedData.bump).toBe(testBump);
            const encodedData = getAddOperatorInstructionDataCodec().encode({
                bump: testBump,
            });
            expect(encodedData).toEqual(instruction.data);
        });

        it('should handle bump parameter correctly', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const admin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);

            // Get a valid operator PDA to use (so auto-derivation doesn't override our bump)
            const operatorPda = await findOperatorPda({
                instance: TEST_ADDRESSES.INSTANCE,
                wallet: TEST_ADDRESSES.OPERATOR,
            });

            // Test various valid u8 values (0-255)
            const testBumps = [1, 42, 127, 200, 254, 255];

            for (const testBump of testBumps) {
                const instruction = await getAddOperatorInstructionAsync({
                    payer,
                    admin,
                    instance: TEST_ADDRESSES.INSTANCE,
                    operator: TEST_ADDRESSES.OPERATOR,
                    operatorPda,
                    bump: testBump,
                });

                const decodedData = getAddOperatorInstructionDataCodec().decode(instruction.data);
                expect(decodedData.bump).toBe(testBump);
            }
        });

        it('should decode instruction data correctly', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const admin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);
            const testBump = 150;

            // Create instruction with specific data
            const instruction = await getAddOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE,
                operator: TEST_ADDRESSES.WALLET,
                bump: testBump,
            });

            // Decode the instruction data
            const decodedData = getAddOperatorInstructionDataCodec().decode(instruction.data);

            // Verify all fields are decoded correctly
            expect(decodedData.discriminator).toBe(ADD_OPERATOR_DISCRIMINATOR);
            expect(decodedData.bump).toBe(testBump);

            // Verify data types
            expect(typeof decodedData.discriminator).toBe('number');
            expect(typeof decodedData.bump).toBe('number');

            // Re-encode and verify it matches
            const reEncodedData = getAddOperatorInstructionDataCodec().encode({
                bump: testBump,
            });
            expect(reEncodedData).toEqual(instruction.data);
        });
    });

    describe('Account requirements', () => {
        it('should include all required accounts: payer, admin, instance, operator, operatorPda, systemProgram, eventAuthority, privateChannelEscrowProgram', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const admin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);

            const instruction = await getAddOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE,
                operator: TEST_ADDRESSES.OPERATOR,
                bump: 42,
            });

            // Based on program instruction definition, AddOperator should have 8 accounts
            expect(instruction.accounts).toHaveLength(8);

            // Account 0: payer (WritableSigner)
            const payerAccount = instruction.accounts[0];
            expect(payerAccount.address).toBe(TEST_ADDRESSES.PAYER);

            // Account 1: admin (ReadonlySigner)
            const adminAccount = instruction.accounts[1];
            expect(adminAccount.address).toBe(TEST_ADDRESSES.ADMIN);

            // Account 2: instance (Readonly)
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

            const instruction = await getAddOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE,
                operator: TEST_ADDRESSES.OPERATOR,
                bump: 42,
            });

            // Account 0: payer - should be WritableSigner
            const payerAccount = instruction.accounts[0];
            expect(payerAccount.role).toBe(AccountRole.WRITABLE_SIGNER);

            // Account 1: admin - should be ReadonlySigner
            const adminAccount = instruction.accounts[1];
            expect(adminAccount.role).toBe(AccountRole.READONLY_SIGNER);

            // Account 2: instance - should be Readonly
            const instanceAccount = instruction.accounts[2];
            expect(instanceAccount.role).toBe(AccountRole.READONLY);

            // Account 3: operator - should be Readonly
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

            const instruction = await getAddOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE,
                operator: TEST_ADDRESSES.OPERATOR,
                bump: 42,
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
        it('should automatically derive operator PDA and bump when not provided', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const admin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);

            // Get expected operator PDA using findOperatorPda
            const [expectedOperatorPda, expectedBump] = await findOperatorPda({
                instance: TEST_ADDRESSES.INSTANCE,
                wallet: TEST_ADDRESSES.OPERATOR,
            });

            // Generate instruction without providing operatorPda - should be auto-derived
            const instruction = await getAddOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE,
                operator: TEST_ADDRESSES.OPERATOR,
                // Not providing bump - should be auto-derived from operatorPda
                // Not providing operatorPda - should be auto-derived
            });

            // Verify the automatically derived operator PDA matches expected address
            expect(instruction.accounts[4].address).toBe(expectedOperatorPda);
            const decodedData = getAddOperatorInstructionDataCodec().decode(instruction.data);
            expect(decodedData.bump).toBe(expectedBump);
            expect(decodedData.discriminator).toBe(ADD_OPERATOR_DISCRIMINATOR);
        });

        it('should use provided operator PDA when supplied (override auto-derivation)', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const admin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);
            const mockBump = 100;

            // Use a different operator PDA for a different wallet
            const overriddenOperatorPda = await findOperatorPda({
                instance: TEST_ADDRESSES.INSTANCE,
                wallet: TEST_ADDRESSES.WALLET,
            });

            const instruction = await getAddOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE,
                operator: TEST_ADDRESSES.OPERATOR,
                operatorPda: overriddenOperatorPda,
                bump: mockBump,
            });

            // Verify the provided address is used instead of auto-derived one
            expect(instruction.accounts[4].address).toBe(overriddenOperatorPda[0]);
            const decodedData = getAddOperatorInstructionDataCodec().decode(instruction.data);
            expect(decodedData.bump).toBe(mockBump);
            expect(decodedData.discriminator).toBe(ADD_OPERATOR_DISCRIMINATOR);
        });

        it('should derive different PDAs for different instance/wallet combinations', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const admin = mockTransactionSigner(TEST_ADDRESSES.ADMIN);

            // Test with different instance/wallet combinations
            const instruction1 = await getAddOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE,
                operator: TEST_ADDRESSES.OPERATOR,
                bump: 42,
            });

            const instruction2 = await getAddOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE,
                operator: TEST_ADDRESSES.WALLET,
                bump: 42,
            });

            const instruction3 = await getAddOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE_SEED,
                operator: TEST_ADDRESSES.OPERATOR,
                bump: 42,
            });

            const instruction4 = await getAddOperatorInstructionAsync({
                payer,
                admin,
                instance: TEST_ADDRESSES.INSTANCE_SEED,
                operator: TEST_ADDRESSES.WALLET,
                bump: 42,
            });

            // All PDAs should be different
            const pdas = [
                instruction1.accounts[4].address,
                instruction2.accounts[4].address,
                instruction3.accounts[4].address,
                instruction4.accounts[4].address,
            ];

            // Check that all PDAs are unique
            const uniquePdas = new Set(pdas);
            expect(uniquePdas.size).toBe(4);
        });
    });
});
