import { Codama, createFromJson } from 'codama';
import {
    appendAccountDiscriminator,
    appendPdaDerivers,
    setInstructionAccountDefaultValues,
    updateInstructionBumps,
} from './updates/';
import { removeEmitInstruction } from './updates/remove-emit-instruction';

export class ContraEscrowCodamaBuilder {
    private codama: Codama;

    constructor(contraEscrowIdl: any) {
        const idlJson = typeof contraEscrowIdl === 'string' ? contraEscrowIdl : JSON.stringify(contraEscrowIdl);
        this.codama = createFromJson(idlJson);
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

export function createContraEscrowCodamaBuilder(contraEscrowIdl: any): ContraEscrowCodamaBuilder {
    return new ContraEscrowCodamaBuilder(contraEscrowIdl);
}
