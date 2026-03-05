import { Codama, createFromJson } from 'codama';
import { setInstructionAccountDefaultValues } from './updates';

export class ContraWithdrawCodamaBuilder {
    private codama: Codama;

    constructor(contraWithdrawIdl: any) {
        const idlJson = typeof contraWithdrawIdl === 'string' ? contraWithdrawIdl : JSON.stringify(contraWithdrawIdl);
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

export function createContraWithdrawCodamaBuilder(contraWithdrawIdl: any): ContraWithdrawCodamaBuilder {
    return new ContraWithdrawCodamaBuilder(contraWithdrawIdl);
}
