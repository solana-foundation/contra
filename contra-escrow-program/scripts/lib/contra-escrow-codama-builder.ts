import { Codama, createFromRoot } from 'codama';
import { AnchorIdl, rootNodeFromAnchor } from '@codama/nodes-from-anchor';
import {
    appendAccountDiscriminator,
    appendPdaDerivers,
    setInstructionAccountDefaultValues,
    updateInstructionBumps,
} from './updates/';
import { removeEmitInstruction } from './updates/remove-emit-instruction';

export class ContraEscrowCodamaBuilder {
    private codama: Codama;

    constructor(contraEscrowIdl: AnchorIdl) {
        this.codama = createFromRoot(rootNodeFromAnchor(contraEscrowIdl));
    }

    appendAccountDiscriminator(): this {
        this.codama = appendAccountDiscriminator(this.codama);
        return this;
    }

    appendPdaDerivers(): this {
        this.codama = appendPdaDerivers(this.codama);
        return this;
    }

    setInstructionAccountDefaultValues(): this {
        this.codama = setInstructionAccountDefaultValues(this.codama);
        return this;
    }

    updateInstructionBumps(): this {
        this.codama = updateInstructionBumps(this.codama);
        return this;
    }

    removeEmitInstruction(): this {
        this.codama = removeEmitInstruction(this.codama);
        return this;
    }

    build(): Codama {
        return this.codama;
    }
}

export function createContraEscrowCodamaBuilder(contraEscrowIdl: AnchorIdl): ContraEscrowCodamaBuilder {
    return new ContraEscrowCodamaBuilder(contraEscrowIdl);
}
