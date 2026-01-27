import { Codama, updateInstructionsVisitor, accountBumpValueNode } from 'codama';

export function updateInstructionBumps(contraEscrowCodama: Codama): Codama {
    contraEscrowCodama.update(
        updateInstructionsVisitor({
            createInstance: {
                arguments: {
                    bump: {
                        defaultValue: accountBumpValueNode('instance'),
                    },
                },
            },
            allowMint: {
                arguments: {
                    bump: {
                        defaultValue: accountBumpValueNode('allowedMint'),
                    },
                },
            },
            addOperator: {
                arguments: {
                    bump: {
                        defaultValue: accountBumpValueNode('operatorPda'),
                    },
                },
            },
        }),
    );
    return contraEscrowCodama;
}
