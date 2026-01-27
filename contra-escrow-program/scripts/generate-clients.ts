import path from 'path';
import { preserveConfigFiles } from './lib/utils';
import { createContraEscrowCodamaBuilder } from './lib/contra-escrow-codama-builder';
import { renderVisitor as renderRustVisitor } from '@codama/renderers-rust';
import { renderVisitor as renderJavaScriptVisitor } from '@codama/renderers-js';

const projectRoot = path.join(__dirname, '..');
const idlDir = path.join(projectRoot, 'idl');
const contraEscrowIdl = require(path.join(idlDir, 'contra_escrow_program.json'));
const rustClientsDir = path.join(__dirname, '..', 'clients', 'rust');
const typescriptClientsDir = path.join(__dirname, '..', 'clients', 'typescript');

// Create and configure the codama instance using the builder pattern
const contraEscrowCodama = createContraEscrowCodamaBuilder(contraEscrowIdl)
    .appendAccountDiscriminator()
    .appendPdaDerivers()
    .setInstructionAccountDefaultValues()
    .updateInstructionBumps()
    .removeEmitInstruction()
    .build();

// Preserve configuration files during generation
const configPreserver = preserveConfigFiles(typescriptClientsDir, rustClientsDir);

// Generate Rust client
contraEscrowCodama.accept(
    renderRustVisitor(path.join(rustClientsDir, 'src', 'generated'), {
        formatCode: true,
        crateFolder: rustClientsDir,
        deleteFolderBeforeRendering: true,
    }),
);

// Generate TypeScript client
contraEscrowCodama.accept(
    renderJavaScriptVisitor(path.join(typescriptClientsDir, 'src', 'generated'), {
        formatCode: true,
        deleteFolderBeforeRendering: true,
    }),
);

// Restore configuration files after generation
configPreserver.restore();
