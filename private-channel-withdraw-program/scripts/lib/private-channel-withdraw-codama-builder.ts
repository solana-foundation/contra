import { Codama, createFromJson } from 'codama';
import { setInstructionAccountDefaultValues } from './updates';

export class PrivateChannelWithdrawCodamaBuilder {
    private codama: Codama;

    constructor(privateChannelWithdrawIdl: any) {
        const idlJson = typeof privateChannelWithdrawIdl === 'string' ? privateChannelWithdrawIdl : JSON.stringify(privateChannelWithdrawIdl);
        this.codama = createFromJson(idlJson);
    }

    setInstructionAccountDefaultValues(): this {
        this.codama = setInstructionAccountDefaultValues(this.codama);
        return this;
    }

    build(): Codama {
        return this.codama;
    }
}

export function createPrivateChannelWithdrawCodamaBuilder(privateChannelWithdrawIdl: any): PrivateChannelWithdrawCodamaBuilder {
    return new PrivateChannelWithdrawCodamaBuilder(privateChannelWithdrawIdl);
}
