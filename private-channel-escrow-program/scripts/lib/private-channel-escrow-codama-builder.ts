import { Codama, createFromJson } from 'codama';
import {
    appendAccountDiscriminator,
    appendPdaDerivers,
    setInstructionAccountDefaultValues,
    updateInstructionBumps,
} from './updates/';
import { removeEmitInstruction } from './updates/remove-emit-instruction';

export class PrivateChannelEscrowCodamaBuilder {
    private codama: Codama;

    constructor(privateChannelEscrowIdl: any) {
        const idlJson = typeof privateChannelEscrowIdl === 'string' ? privateChannelEscrowIdl : JSON.stringify(privateChannelEscrowIdl);
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

export function createPrivateChannelEscrowCodamaBuilder(privateChannelEscrowIdl: any): PrivateChannelEscrowCodamaBuilder {
    return new PrivateChannelEscrowCodamaBuilder(privateChannelEscrowIdl);
}
