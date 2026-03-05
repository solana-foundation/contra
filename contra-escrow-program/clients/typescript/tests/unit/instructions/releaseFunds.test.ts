import { expect } from '@jest/globals';
import {
    getReleaseFundsInstructionAsync,
    getReleaseFundsInstructionDataCodec,
    RELEASE_FUNDS_DISCRIMINATOR,
    findOperatorPda,
    findAllowedMintPda,
    CONTRA_ESCROW_PROGRAM_PROGRAM_ADDRESS,
} from '../../../src/generated';
import {
    mockTransactionSigner,
    TEST_ADDRESSES,
    EXPECTED_PROGRAM_ADDRESS,
    TEST_ROOT,
    TEST_SIBLING_PROOFS,
    TEST_TRANSACTION_NONCE,
} from '../../setup/mocks';
import { AccountRole, assertIsAddress, type Address } from '@solana/kit';
import { TOKEN_PROGRAM_ADDRESS, ASSOCIATED_TOKEN_PROGRAM_ADDRESS, findAssociatedTokenPda } from '@solana-program/token';
import { TOKEN_2022_PROGRAM_ADDRESS } from '@solana-program/token-2022';

// Token program addresses
describe('releaseFunds', () => {
    describe('Instruction data validation', () => {
        it('should encode instruction data with correct discriminator (7), amount, user, and newWithdrawalRoot', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const operator = mockTransactionSigner(TEST_ADDRESSES.OPERATOR);
            const testAmount = BigInt(1000000); // 1 USDC (6 decimals)
            const testUser = TEST_ADDRESSES.WALLET;
            const testWithdrawalRoot = TEST_ROOT;

            const instruction = await getReleaseFundsInstructionAsync({
                payer,
                operator,
                instance: TEST_ADDRESSES.INSTANCE,
                mint: TEST_ADDRESSES.USDC_MINT,
                userAta: TEST_ADDRESSES.INSTANCE_ATA,
                amount: testAmount,
                user: testUser,
                newWithdrawalRoot: testWithdrawalRoot,
                transactionNonce: TEST_TRANSACTION_NONCE,
                siblingProofs: TEST_SIBLING_PROOFS,
            });

            const decodedData = getReleaseFundsInstructionDataCodec().decode(instruction.data);

            // Verify discriminator is 7 as defined in the program
            expect(decodedData.discriminator).toBe(RELEASE_FUNDS_DISCRIMINATOR);
            expect(decodedData.discriminator).toBe(7);

            // Verify amount is correctly encoded as bigint
            expect(decodedData.amount).toBe(testAmount);
            expect(typeof decodedData.amount).toBe('bigint');

            // Verify user address is correctly encoded
            expect(decodedData.user).toBe(testUser);
            assertIsAddress(decodedData.user);

            // Verify withdrawal root is correctly encoded (32 bytes)
            expect(decodedData.newWithdrawalRoot).toEqual(Array.from(testWithdrawalRoot));
            expect(decodedData.newWithdrawalRoot).toHaveLength(32);
        });

        it('should handle amount parameter correctly (u64 as bigint)', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const operator = mockTransactionSigner(TEST_ADDRESSES.OPERATOR);
            const testUser = TEST_ADDRESSES.WALLET as Address;
            const testWithdrawalRoot = TEST_ROOT;

            // Test various valid u64 values as both number and bigint
            const testAmounts = [
                0n,
                1n,
                BigInt(1000000), // 1 USDC
                BigInt(1000000000000), // 1 million USDC
                BigInt('18446744073709551615'), // Max u64
                1000000, // number (should be converted to bigint)
            ];

            for (const testAmount of testAmounts) {
                const instruction = await getReleaseFundsInstructionAsync({
                    payer,
                    operator,
                    instance: TEST_ADDRESSES.INSTANCE,
                    mint: TEST_ADDRESSES.USDC_MINT,
                    userAta: TEST_ADDRESSES.INSTANCE_ATA,
                    amount: testAmount,
                    user: testUser,
                    newWithdrawalRoot: testWithdrawalRoot,
                    transactionNonce: TEST_TRANSACTION_NONCE,
                    siblingProofs: TEST_SIBLING_PROOFS,
                });

                const decodedData = getReleaseFundsInstructionDataCodec().decode(instruction.data);
                expect(decodedData.amount).toBe(BigInt(testAmount));
                expect(typeof decodedData.amount).toBe('bigint');
            }
        });

        it('should handle user parameter correctly (Address)', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const operator = mockTransactionSigner(TEST_ADDRESSES.OPERATOR);
            const testAmount = BigInt(1000000);
            const testWithdrawalRoot = Array.from(new Uint8Array(32).fill(0));

            // Test with different valid user addresses
            const testUsers = [
                TEST_ADDRESSES.WALLET,
                TEST_ADDRESSES.ADMIN,
                TEST_ADDRESSES.PAYER,
                TEST_ADDRESSES.OPERATOR,
            ] as Address[];

            for (const testUser of testUsers) {
                const instruction = await getReleaseFundsInstructionAsync({
                    payer,
                    operator,
                    instance: TEST_ADDRESSES.INSTANCE,
                    mint: TEST_ADDRESSES.USDC_MINT,
                    userAta: TEST_ADDRESSES.INSTANCE_ATA,
                    amount: testAmount,
                    user: testUser,
                    newWithdrawalRoot: testWithdrawalRoot,
                    transactionNonce: TEST_TRANSACTION_NONCE,
                    siblingProofs: TEST_SIBLING_PROOFS,
                });

                const decodedData = getReleaseFundsInstructionDataCodec().decode(instruction.data);
                expect(decodedData.user).toBe(testUser);
            }
        });

        it('should handle newWithdrawalRoot parameter correctly (32 bytes)', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const operator = mockTransactionSigner(TEST_ADDRESSES.OPERATOR);
            const testAmount = BigInt(1000000);
            const testUser = TEST_ADDRESSES.WALLET as Address;

            // Test various 32-byte withdrawal roots
            const testRoots = [
                Array.from(new Uint8Array(32).fill(0)), // All zeros
                Array.from(new Uint8Array(32).fill(255)), // All 0xFF
                Array.from({ length: 32 }, (_, i) => i), // Sequential bytes
                Array.from(crypto.getRandomValues(new Uint8Array(32))), // Random bytes
            ];

            for (const testRoot of testRoots) {
                const instruction = await getReleaseFundsInstructionAsync({
                    payer,
                    operator,
                    instance: TEST_ADDRESSES.INSTANCE,
                    mint: TEST_ADDRESSES.USDC_MINT,
                    userAta: TEST_ADDRESSES.INSTANCE_ATA,
                    amount: testAmount,
                    user: testUser,
                    newWithdrawalRoot: testRoot,
                    transactionNonce: TEST_TRANSACTION_NONCE,
                    siblingProofs: TEST_SIBLING_PROOFS,
                });

                const decodedData = getReleaseFundsInstructionDataCodec().decode(instruction.data);
                expect(decodedData.newWithdrawalRoot).toEqual(testRoot);
                expect(decodedData.newWithdrawalRoot).toHaveLength(32);
            }
        });

        it('should decode instruction data correctly', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const operator = mockTransactionSigner(TEST_ADDRESSES.OPERATOR);
            const testAmount = BigInt(2500000000); // 2.5K USDC
            const testUser = TEST_ADDRESSES.ADMIN as Address;
            const testWithdrawalRoot = Array.from(new Uint8Array(32).fill(123));

            // Create instruction with specific data
            const instruction = await getReleaseFundsInstructionAsync({
                payer,
                operator,
                instance: TEST_ADDRESSES.INSTANCE,
                mint: TEST_ADDRESSES.USDC_MINT,
                userAta: TEST_ADDRESSES.INSTANCE_ATA,
                amount: testAmount,
                user: testUser,
                newWithdrawalRoot: testWithdrawalRoot,
                transactionNonce: TEST_TRANSACTION_NONCE,
                siblingProofs: TEST_SIBLING_PROOFS,
            });

            // Decode the instruction data
            const decodedData = getReleaseFundsInstructionDataCodec().decode(instruction.data);

            // Verify all fields are decoded correctly
            expect(decodedData.discriminator).toBe(RELEASE_FUNDS_DISCRIMINATOR);
            expect(decodedData.amount).toBe(testAmount);
            expect(decodedData.user).toBe(testUser);
            expect(decodedData.newWithdrawalRoot).toEqual(Array.from(testWithdrawalRoot));

            // Verify data types
            expect(typeof decodedData.discriminator).toBe('number');
            expect(typeof decodedData.amount).toBe('bigint');
            expect(typeof decodedData.user).toBe('string');
            expect(Array.isArray(decodedData.newWithdrawalRoot)).toBe(true);

            // Re-encode and verify it matches
            const reEncodedData = getReleaseFundsInstructionDataCodec().encode({
                amount: testAmount,
                user: testUser,
                newWithdrawalRoot: testWithdrawalRoot,
                transactionNonce: TEST_TRANSACTION_NONCE,
                siblingProofs: TEST_SIBLING_PROOFS,
            });
            expect(reEncodedData).toEqual(instruction.data);
        });
    });

    describe('Account requirements', () => {
        it('should include all required accounts: payer, operator, instance, operatorPda, mint, allowedMint, userAta, instanceAta, tokenProgram, associatedTokenProgram, eventAuthority, contraEscrowProgram', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const operator = mockTransactionSigner(TEST_ADDRESSES.OPERATOR);
            const testAmount = BigInt(1000000);
            const testUser = TEST_ADDRESSES.WALLET as Address;
            const testWithdrawalRoot = Array.from(new Uint8Array(32).fill(0));

            const instruction = await getReleaseFundsInstructionAsync({
                payer,
                operator,
                instance: TEST_ADDRESSES.INSTANCE,
                mint: TEST_ADDRESSES.USDC_MINT,
                userAta: TEST_ADDRESSES.INSTANCE_ATA,
                amount: testAmount,
                user: testUser,
                newWithdrawalRoot: testWithdrawalRoot,
                transactionNonce: TEST_TRANSACTION_NONCE,
                siblingProofs: TEST_SIBLING_PROOFS,
            });

            // Based on program instruction definition, ReleaseFunds should have 12 accounts
            expect(instruction.accounts).toHaveLength(12);

            // Account 0: payer (WritableSigner)
            const payerAccount = instruction.accounts[0];
            expect(payerAccount.address).toBe(TEST_ADDRESSES.PAYER);

            // Account 1: operator (ReadonlySigner)
            const operatorAccount = instruction.accounts[1];
            expect(operatorAccount.address).toBe(TEST_ADDRESSES.OPERATOR);

            // Account 2: instance (Writable)
            const instanceAccount = instruction.accounts[2];
            expect(instanceAccount.address).toBe(TEST_ADDRESSES.INSTANCE);

            // Account 3: operatorPda (Readonly PDA - auto-derived)
            const operatorPdaAccount = instruction.accounts[3];
            expect(operatorPdaAccount.address).toBeDefined();

            // Account 4: mint (Readonly)
            const mintAccount = instruction.accounts[4];
            expect(mintAccount.address).toBe(TEST_ADDRESSES.USDC_MINT);

            // Account 5: allowedMint (Readonly PDA - auto-derived)
            const allowedMintAccount = instruction.accounts[5];
            expect(allowedMintAccount.address).toBeDefined();

            // Account 6: userAta (Writable)
            const userAtaAccount = instruction.accounts[6];
            expect(userAtaAccount.address).toBe(TEST_ADDRESSES.INSTANCE_ATA);

            // Account 7: instanceAta (Writable - auto-derived)
            const instanceAtaAccount = instruction.accounts[7];
            expect(instanceAtaAccount.address).toBeDefined();

            // Account 8: tokenProgram (Readonly)
            const tokenProgramAccount = instruction.accounts[8];
            expect(tokenProgramAccount.address).toBe(TOKEN_PROGRAM_ADDRESS);

            // Account 9: associatedTokenProgram (Readonly)
            const associatedTokenProgramAccount = instruction.accounts[9];
            expect(associatedTokenProgramAccount.address).toBe(ASSOCIATED_TOKEN_PROGRAM_ADDRESS);

            // Account 10: eventAuthority (Readonly)
            const eventAuthorityAccount = instruction.accounts[10];
            expect(eventAuthorityAccount.address).toBeDefined();

            // Account 11: contraEscrowProgram (Readonly)
            const contraEscrowProgramAccount = instruction.accounts[11];
            expect(contraEscrowProgramAccount.address).toBe(CONTRA_ESCROW_PROGRAM_PROGRAM_ADDRESS);
        });

        it('should set correct account permissions (writable/readable/signer)', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const operator = mockTransactionSigner(TEST_ADDRESSES.OPERATOR);
            const testAmount = BigInt(1000000);
            const testUser = TEST_ADDRESSES.WALLET as Address;
            const testWithdrawalRoot = Array.from(new Uint8Array(32).fill(0));

            const instruction = await getReleaseFundsInstructionAsync({
                payer,
                operator,
                instance: TEST_ADDRESSES.INSTANCE,
                mint: TEST_ADDRESSES.USDC_MINT,
                userAta: TEST_ADDRESSES.INSTANCE_ATA,
                amount: testAmount,
                user: testUser,
                newWithdrawalRoot: testWithdrawalRoot,
                transactionNonce: TEST_TRANSACTION_NONCE,
                siblingProofs: TEST_SIBLING_PROOFS,
            });

            // Account 0: payer - should be WritableSigner
            const payerAccount = instruction.accounts[0];
            expect(payerAccount.role).toBe(AccountRole.WRITABLE_SIGNER);

            // Account 1: operator - should be ReadonlySigner
            const operatorAccount = instruction.accounts[1];
            expect(operatorAccount.role).toBe(AccountRole.READONLY_SIGNER);

            // Account 2: instance - should be Writable (PDA, not a signer)
            const instanceAccount = instruction.accounts[2];
            expect(instanceAccount.role).toBe(AccountRole.WRITABLE);

            // Account 3: operatorPda - should be Readonly (PDA, not a signer)
            const operatorPdaAccount = instruction.accounts[3];
            expect(operatorPdaAccount.role).toBe(AccountRole.READONLY);

            // Account 4: mint - should be Readonly
            const mintAccount = instruction.accounts[4];
            expect(mintAccount.role).toBe(AccountRole.READONLY);

            // Account 5: allowedMint - should be Readonly (PDA, not a signer)
            const allowedMintAccount = instruction.accounts[5];
            expect(allowedMintAccount.role).toBe(AccountRole.READONLY);

            // Account 6: userAta - should be Writable
            const userAtaAccount = instruction.accounts[6];
            expect(userAtaAccount.role).toBe(AccountRole.WRITABLE);

            // Account 7: instanceAta - should be Writable (ATA, not a signer)
            const instanceAtaAccount = instruction.accounts[7];
            expect(instanceAtaAccount.role).toBe(AccountRole.WRITABLE);

            // Account 8: tokenProgram - should be Readonly
            const tokenProgramAccount = instruction.accounts[8];
            expect(tokenProgramAccount.role).toBe(AccountRole.READONLY);

            // Account 9: associatedTokenProgram - should be Readonly
            const associatedTokenProgramAccount = instruction.accounts[9];
            expect(associatedTokenProgramAccount.role).toBe(AccountRole.READONLY);

            // Account 10: eventAuthority - should be Readonly (PDA, not a signer)
            const eventAuthorityAccount = instruction.accounts[10];
            expect(eventAuthorityAccount.role).toBe(AccountRole.READONLY);

            // Account 11: contraEscrowProgram - should be Readonly
            const contraEscrowProgramAccount = instruction.accounts[11];
            expect(contraEscrowProgramAccount.role).toBe(AccountRole.READONLY);
        });

        it('should use correct program addresses', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const operator = mockTransactionSigner(TEST_ADDRESSES.OPERATOR);
            const testAmount = BigInt(1000000);
            const testUser = TEST_ADDRESSES.WALLET as Address;
            const testWithdrawalRoot = Array.from(new Uint8Array(32).fill(0));

            const instruction = await getReleaseFundsInstructionAsync({
                payer,
                operator,
                instance: TEST_ADDRESSES.INSTANCE,
                mint: TEST_ADDRESSES.USDC_MINT,
                userAta: TEST_ADDRESSES.INSTANCE_ATA,
                amount: testAmount,
                user: testUser,
                newWithdrawalRoot: testWithdrawalRoot,
                transactionNonce: TEST_TRANSACTION_NONCE,
                siblingProofs: TEST_SIBLING_PROOFS,
            });

            // Verify the instruction uses the correct program address
            expect(instruction.programAddress).toBe(CONTRA_ESCROW_PROGRAM_PROGRAM_ADDRESS);
            expect(instruction.programAddress).toBe(EXPECTED_PROGRAM_ADDRESS);

            // Verify tokenProgram uses the correct address
            const tokenProgramAccount = instruction.accounts[8];
            expect(tokenProgramAccount.address).toBe(TOKEN_PROGRAM_ADDRESS);

            // Verify associatedTokenProgram uses the correct address
            const associatedTokenProgramAccount = instruction.accounts[9];
            expect(associatedTokenProgramAccount.address).toBe(ASSOCIATED_TOKEN_PROGRAM_ADDRESS);

            // Verify contraEscrowProgram uses the correct address
            const contraEscrowProgramAccount = instruction.accounts[11];
            expect(contraEscrowProgramAccount.address).toBe(CONTRA_ESCROW_PROGRAM_PROGRAM_ADDRESS);
        });
    });

    describe('Automatic PDA derivation', () => {
        it('should automatically derive operatorPda when not provided', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const operator = mockTransactionSigner(TEST_ADDRESSES.OPERATOR);
            const testAmount = BigInt(1000000);
            const testUser = TEST_ADDRESSES.WALLET as Address;
            const testWithdrawalRoot = Array.from(new Uint8Array(32).fill(0));

            // Get expected operator PDA using findOperatorPda
            const [expectedOperatorPda] = await findOperatorPda({
                instance: TEST_ADDRESSES.INSTANCE,
                wallet: TEST_ADDRESSES.OPERATOR,
            });

            // Generate instruction without providing operatorPda - should be auto-derived
            const instruction = await getReleaseFundsInstructionAsync({
                payer,
                operator,
                instance: TEST_ADDRESSES.INSTANCE,
                mint: TEST_ADDRESSES.USDC_MINT,
                userAta: TEST_ADDRESSES.INSTANCE_ATA,
                amount: testAmount,
                user: testUser,
                newWithdrawalRoot: testWithdrawalRoot,
                transactionNonce: TEST_TRANSACTION_NONCE,
                siblingProofs: TEST_SIBLING_PROOFS,
                // Not providing operatorPda - should be auto-derived
            });

            // Verify the automatically derived operatorPda matches expected address
            expect(instruction.accounts[3].address).toBe(expectedOperatorPda);
        });

        it('should automatically derive allowedMint when not provided', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const operator = mockTransactionSigner(TEST_ADDRESSES.OPERATOR);
            const testAmount = BigInt(1000000);
            const testUser = TEST_ADDRESSES.WALLET as Address;
            const testWithdrawalRoot = Array.from(new Uint8Array(32).fill(0));

            // Get expected allowed mint PDA using findAllowedMintPda
            const [expectedAllowedMintPda] = await findAllowedMintPda({
                instance: TEST_ADDRESSES.INSTANCE,
                mint: TEST_ADDRESSES.USDC_MINT,
            });

            // Generate instruction without providing allowedMint - should be auto-derived
            const instruction = await getReleaseFundsInstructionAsync({
                payer,
                operator,
                instance: TEST_ADDRESSES.INSTANCE,
                mint: TEST_ADDRESSES.USDC_MINT,
                userAta: TEST_ADDRESSES.INSTANCE_ATA,
                amount: testAmount,
                user: testUser,
                newWithdrawalRoot: testWithdrawalRoot,
                transactionNonce: TEST_TRANSACTION_NONCE,
                siblingProofs: TEST_SIBLING_PROOFS,
                // Not providing allowedMint - should be auto-derived
            });

            // Verify the automatically derived allowedMint matches expected address
            expect(instruction.accounts[5].address).toBe(expectedAllowedMintPda);
        });

        it('should automatically derive instanceAta when not provided', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const operator = mockTransactionSigner(TEST_ADDRESSES.OPERATOR);
            const testAmount = BigInt(1000000);
            const testUser = TEST_ADDRESSES.WALLET as Address;
            const testWithdrawalRoot = Array.from(new Uint8Array(32).fill(0));

            const [expectedInstanceAta] = await findAssociatedTokenPda({
                mint: TEST_ADDRESSES.USDC_MINT,
                owner: TEST_ADDRESSES.INSTANCE,
                tokenProgram: TOKEN_PROGRAM_ADDRESS,
            });

            // Generate instruction without providing instanceAta - should be auto-derived
            const instruction = await getReleaseFundsInstructionAsync({
                payer,
                operator,
                instance: TEST_ADDRESSES.INSTANCE,
                mint: TEST_ADDRESSES.USDC_MINT,
                userAta: TEST_ADDRESSES.INSTANCE_ATA,
                amount: testAmount,
                user: testUser,
                newWithdrawalRoot: testWithdrawalRoot,
                transactionNonce: TEST_TRANSACTION_NONCE,
                siblingProofs: TEST_SIBLING_PROOFS,
                // Not providing instanceAta - should be auto-derived as ATA
            });
            // Verify instanceAta is derived (should be a valid address)
            const instanceAtaAccount = instruction.accounts[7];
            expect(instanceAtaAccount.address).toBe(expectedInstanceAta);
        });
        it('should automatically derive instanceAta when not provided (token 2022)', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const operator = mockTransactionSigner(TEST_ADDRESSES.OPERATOR);
            const testAmount = BigInt(1000000);
            const testUser = TEST_ADDRESSES.WALLET as Address;
            const testWithdrawalRoot = Array.from(new Uint8Array(32).fill(0));

            const [expectedInstanceAta] = await findAssociatedTokenPda({
                mint: TEST_ADDRESSES.USDC_MINT,
                owner: TEST_ADDRESSES.INSTANCE,
                tokenProgram: TOKEN_2022_PROGRAM_ADDRESS,
            });

            // Generate instruction without providing instanceAta - should be auto-derived
            const instruction = await getReleaseFundsInstructionAsync({
                payer,
                operator,
                instance: TEST_ADDRESSES.INSTANCE,
                mint: TEST_ADDRESSES.USDC_MINT,
                userAta: TEST_ADDRESSES.INSTANCE_ATA,
                amount: testAmount,
                user: testUser,
                tokenProgram: TOKEN_2022_PROGRAM_ADDRESS,
                newWithdrawalRoot: testWithdrawalRoot,
                transactionNonce: TEST_TRANSACTION_NONCE,
                siblingProofs: TEST_SIBLING_PROOFS,
                // Not providing instanceAta - should be auto-derived as ATA
            });

            // Verify the automatically derived instance ATA is defined
            const instanceAtaAccount = instruction.accounts[7];
            expect(instanceAtaAccount.address).toBe(expectedInstanceAta);

            // Verify Token 2022 program is used
            const tokenProgramAccount = instruction.accounts[8];
            expect(tokenProgramAccount.address).toBe(TOKEN_2022_PROGRAM_ADDRESS);
        });

        it('should use provided PDAs when supplied (override auto-derivation)', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const operator = mockTransactionSigner(TEST_ADDRESSES.OPERATOR);
            const testAmount = BigInt(1000000);
            const testUser = TEST_ADDRESSES.WALLET as Address;
            const testWithdrawalRoot = Array.from(new Uint8Array(32).fill(0));

            // Use different addresses to override auto-derivation
            const overriddenOperatorPda = TEST_ADDRESSES.OPERATOR; // Use as override
            const overriddenAllowedMint = TEST_ADDRESSES.ALLOWED_MINT; // Use as override
            const overriddenInstanceAta = TEST_ADDRESSES.INSTANCE_ATA; // Use as override

            const instruction = await getReleaseFundsInstructionAsync({
                payer,
                operator,
                instance: TEST_ADDRESSES.INSTANCE,
                operatorPda: overriddenOperatorPda,
                mint: TEST_ADDRESSES.USDC_MINT,
                allowedMint: overriddenAllowedMint,
                userAta: TEST_ADDRESSES.INSTANCE_ATA,
                instanceAta: overriddenInstanceAta,
                amount: testAmount,
                user: testUser,
                newWithdrawalRoot: testWithdrawalRoot,
                transactionNonce: TEST_TRANSACTION_NONCE,
                siblingProofs: TEST_SIBLING_PROOFS,
            });

            // Verify the provided addresses are used instead of auto-derived ones
            expect(instruction.accounts[3].address).toBe(overriddenOperatorPda);
            expect(instruction.accounts[5].address).toBe(overriddenAllowedMint);
            expect(instruction.accounts[7].address).toBe(overriddenInstanceAta);
        });
    });

    describe('Operator validation', () => {
        it('should require operator to be a signer', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const operator = mockTransactionSigner(TEST_ADDRESSES.OPERATOR);
            const testAmount = BigInt(1000000);
            const testUser = TEST_ADDRESSES.WALLET as Address;
            const testWithdrawalRoot = Array.from(new Uint8Array(32).fill(0));

            const instruction = await getReleaseFundsInstructionAsync({
                payer,
                operator,
                instance: TEST_ADDRESSES.INSTANCE,
                mint: TEST_ADDRESSES.USDC_MINT,
                userAta: TEST_ADDRESSES.INSTANCE_ATA,
                amount: testAmount,
                user: testUser,
                newWithdrawalRoot: testWithdrawalRoot,
                transactionNonce: TEST_TRANSACTION_NONCE,
                siblingProofs: TEST_SIBLING_PROOFS,
            });

            // Verify operator account is a signer
            const operatorAccount = instruction.accounts[1];
            expect(operatorAccount.role).toBe(AccountRole.READONLY_SIGNER);
            expect(operatorAccount.address).toBe(TEST_ADDRESSES.OPERATOR);
        });

        it('should handle different operator addresses', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const testAmount = BigInt(1000000);
            const testUser = TEST_ADDRESSES.WALLET as Address;
            const testWithdrawalRoot = Array.from(new Uint8Array(32).fill(0));

            // Test with different valid operator addresses
            const testOperators = [
                mockTransactionSigner(TEST_ADDRESSES.OPERATOR),
                mockTransactionSigner(TEST_ADDRESSES.ADMIN),
                mockTransactionSigner(TEST_ADDRESSES.WALLET),
            ];

            for (const operator of testOperators) {
                const instruction = await getReleaseFundsInstructionAsync({
                    payer,
                    operator,
                    instance: TEST_ADDRESSES.INSTANCE,
                    mint: TEST_ADDRESSES.USDC_MINT,
                    userAta: TEST_ADDRESSES.INSTANCE_ATA,
                    amount: testAmount,
                    user: testUser,
                    newWithdrawalRoot: testWithdrawalRoot,
                    transactionNonce: TEST_TRANSACTION_NONCE,
                    siblingProofs: TEST_SIBLING_PROOFS,
                });

                // Verify operator account uses the correct address
                const operatorAccount = instruction.accounts[1];
                expect(operatorAccount.address).toBe(operator.address);
                expect(operatorAccount.role).toBe(AccountRole.READONLY_SIGNER);
            }
        });

        it('should automatically derive operatorPda based on operator address', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const testAmount = BigInt(1000000);
            const testUser = TEST_ADDRESSES.WALLET as Address;
            const testWithdrawalRoot = Array.from(new Uint8Array(32).fill(0));

            // Test with different operators and verify operatorPda derives correctly
            const testOperators = [
                mockTransactionSigner(TEST_ADDRESSES.OPERATOR),
                mockTransactionSigner(TEST_ADDRESSES.ADMIN),
            ];

            for (const operator of testOperators) {
                // Get expected operator PDA for this operator
                const [expectedOperatorPda] = await findOperatorPda({
                    instance: TEST_ADDRESSES.INSTANCE,
                    wallet: operator.address,
                });

                const instruction = await getReleaseFundsInstructionAsync({
                    payer,
                    operator,
                    instance: TEST_ADDRESSES.INSTANCE,
                    mint: TEST_ADDRESSES.USDC_MINT,
                    userAta: TEST_ADDRESSES.INSTANCE_ATA,
                    amount: testAmount,
                    user: testUser,
                    newWithdrawalRoot: testWithdrawalRoot,
                    transactionNonce: TEST_TRANSACTION_NONCE,
                    siblingProofs: TEST_SIBLING_PROOFS,
                });

                // Verify operatorPda is derived correctly for this operator
                const operatorPdaAccount = instruction.accounts[3];
                expect(operatorPdaAccount.address).toBe(expectedOperatorPda);
            }
        });
    });

    describe('Parameter edge cases', () => {
        it('should handle zero amount', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const operator = mockTransactionSigner(TEST_ADDRESSES.OPERATOR);
            const testAmount = BigInt(0);
            const testUser = TEST_ADDRESSES.WALLET as Address;
            const testWithdrawalRoot = Array.from(new Uint8Array(32).fill(0));

            const instruction = await getReleaseFundsInstructionAsync({
                payer,
                operator,
                instance: TEST_ADDRESSES.INSTANCE,
                mint: TEST_ADDRESSES.USDC_MINT,
                userAta: TEST_ADDRESSES.INSTANCE_ATA,
                amount: testAmount,
                user: testUser,
                newWithdrawalRoot: testWithdrawalRoot,
                transactionNonce: TEST_TRANSACTION_NONCE,
                siblingProofs: TEST_SIBLING_PROOFS,
            });

            const decodedData = getReleaseFundsInstructionDataCodec().decode(instruction.data);
            expect(decodedData.amount).toBe(BigInt(0));
        });

        it('should handle maximum u64 amount', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const operator = mockTransactionSigner(TEST_ADDRESSES.OPERATOR);
            const testAmount = BigInt('18446744073709551615'); // Max u64
            const testUser = TEST_ADDRESSES.WALLET as Address;
            const testWithdrawalRoot = Array.from(new Uint8Array(32).fill(0));

            const instruction = await getReleaseFundsInstructionAsync({
                payer,
                operator,
                instance: TEST_ADDRESSES.INSTANCE,
                mint: TEST_ADDRESSES.USDC_MINT,
                userAta: TEST_ADDRESSES.INSTANCE_ATA,
                amount: testAmount,
                user: testUser,
                newWithdrawalRoot: testWithdrawalRoot,
                transactionNonce: TEST_TRANSACTION_NONCE,
                siblingProofs: TEST_SIBLING_PROOFS,
            });

            const decodedData = getReleaseFundsInstructionDataCodec().decode(instruction.data);
            expect(decodedData.amount).toBe(testAmount);
        });

        it('should handle different withdrawal root patterns', async () => {
            const payer = mockTransactionSigner(TEST_ADDRESSES.PAYER);
            const operator = mockTransactionSigner(TEST_ADDRESSES.OPERATOR);
            const testAmount = BigInt(1000000);
            const testUser = TEST_ADDRESSES.WALLET as Address;

            // Test edge case withdrawal roots
            const testRoots = [
                Array.from(new Uint8Array(32).fill(0)), // All zeros
                Array.from(new Uint8Array(32).fill(255)), // All 0xFF
                [...Array(32).keys()], // 0-31 pattern
            ];

            for (const testRoot of testRoots) {
                const instruction = await getReleaseFundsInstructionAsync({
                    payer,
                    operator,
                    instance: TEST_ADDRESSES.INSTANCE,
                    mint: TEST_ADDRESSES.USDC_MINT,
                    userAta: TEST_ADDRESSES.INSTANCE_ATA,
                    amount: testAmount,
                    user: testUser,
                    newWithdrawalRoot: testRoot,
                    transactionNonce: TEST_TRANSACTION_NONCE,
                    siblingProofs: TEST_SIBLING_PROOFS,
                });

                const decodedData = getReleaseFundsInstructionDataCodec().decode(instruction.data);
                expect(decodedData.newWithdrawalRoot).toEqual(testRoot);
            }
        });
    });
});
