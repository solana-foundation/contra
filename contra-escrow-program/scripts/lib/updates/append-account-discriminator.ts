import { Codama, bottomUpTransformerVisitor, structFieldTypeNode, numberTypeNode, assertIsNode, isNode } from 'codama';

export function appendAccountDiscriminator(contraEscrowCodama: Codama): Codama {
    contraEscrowCodama.update(
        bottomUpTransformerVisitor([
            {
                select: '[accountNode]',
                transform: node => {
                    assertIsNode(node, 'accountNode');

                    if (isNode(node.data, 'structTypeNode')) {
                        const updatedNode = {
                            ...node,
                            data: {
                                ...node.data,
                                fields: [
                                    structFieldTypeNode({
                                        name: 'discriminator',
                                        type: numberTypeNode('u8'),
                                    }),
                                    ...node.data.fields,
                                ],
                            },
                        };

                        if (node.size !== undefined) {
                            return {
                                ...updatedNode,
                                size: (node.size ?? 0) + 1,
                            };
                        }

                        return updatedNode;
                    }

                    return node;
                },
            },
        ]),
    );
    return contraEscrowCodama;
}
