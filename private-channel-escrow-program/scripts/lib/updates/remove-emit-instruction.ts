import { Codama, updateInstructionsVisitor } from 'codama';

export function removeEmitInstruction(privateChannelEscrowCodama: Codama): Codama {
    privateChannelEscrowCodama.update(
        updateInstructionsVisitor({
            emitEvent: {
                delete: true,
            },
        }),
    );
    return privateChannelEscrowCodama;
}
