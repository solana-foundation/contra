import {
    Codama,
    constantPdaSeedNode,
    stringTypeNode,
    stringValueNode,
    variablePdaSeedNode,
    publicKeyTypeNode,
    addPdasVisitor,
} from 'codama';

export function appendPdaDerivers(contraEscrowCodama: Codama): Codama {
    contraEscrowCodama.update(
        addPdasVisitor({
            contraEscrowProgram: [
                {
                    name: 'instance',
                    seeds: [
                        constantPdaSeedNode(stringTypeNode('utf8'), stringValueNode('instance')),
                        variablePdaSeedNode('instanceSeed', publicKeyTypeNode()),
                    ],
                },
                {
                    name: 'allowedMint',
                    seeds: [
                        constantPdaSeedNode(stringTypeNode('utf8'), stringValueNode('allowed_mint')),
                        variablePdaSeedNode('instance', publicKeyTypeNode()),
                        variablePdaSeedNode('mint', publicKeyTypeNode()),
                    ],
                },
                {
                    name: 'operator',
                    seeds: [
                        constantPdaSeedNode(stringTypeNode('utf8'), stringValueNode('operator')),
                        variablePdaSeedNode('instance', publicKeyTypeNode()),
                        variablePdaSeedNode('wallet', publicKeyTypeNode()),
                    ],
                },
                {
                    name: 'eventAuthority',
                    seeds: [constantPdaSeedNode(stringTypeNode('utf8'), stringValueNode('event_authority'))],
                },
            ],
        }),
    );
    return contraEscrowCodama;
}
