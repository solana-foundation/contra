import { Codama, createFromRoot } from 'codama';
import { AnchorIdl, rootNodeFromAnchor } from '@codama/nodes-from-anchor';
import { setInstructionAccountDefaultValues } from './updates';

export class ContraWithdrawCodamaBuilder {
    private codama: Codama;

    constructor(contraWithdrawIdl: AnchorIdl) {
        this.codama = createFromRoot(rootNodeFromAnchor(contraWithdrawIdl));
    }

    setInstructionAccountDefaultValues(): this {
        this.codama = setInstructionAccountDefaultValues(this.codama);
        return this;
    }

    build(): Codama {
        return this.codama;
    }
}

export function createContraWithdrawCodamaBuilder(contraWithdrawIdl: AnchorIdl): ContraWithdrawCodamaBuilder {
    return new ContraWithdrawCodamaBuilder(contraWithdrawIdl);
}
