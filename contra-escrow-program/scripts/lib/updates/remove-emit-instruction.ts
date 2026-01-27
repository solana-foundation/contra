import { Codama, updateInstructionsVisitor } from 'codama';

export function removeEmitInstruction(contraEscrowCodama: Codama): Codama {
    contraEscrowCodama.update(
        updateInstructionsVisitor({
            emitEvent: {
                delete: true,
            },
        }),
    );
    return contraEscrowCodama;
}
