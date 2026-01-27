import path from 'path';
import { preserveConfigFiles } from './lib/utils';
import { createContraWithdrawCodamaBuilder } from './lib/contra-withdraw-codama-builder';
import { renderVisitor as renderRustVisitor } from '@codama/renderers-rust';
import { renderVisitor as renderJavaScriptVisitor } from '@codama/renderers-js';

const projectRoot = path.join(__dirname, '..');
const idlDir = path.join(projectRoot, 'idl');
const contraWithdrawIdl = require(path.join(idlDir, 'contra_withdraw_program.json'));
const rustClientsDir = path.join(__dirname, '..', 'clients', 'rust');
const typescriptClientsDir = path.join(__dirname, '..', 'clients', 'typescript');

// Create and configure the codama instance using the builder pattern
const contraWithdrawCodama = createContraWithdrawCodamaBuilder(contraWithdrawIdl)
    .setInstructionAccountDefaultValues()
    .build();

// Preserve configuration files during generation
const configPreserver = preserveConfigFiles(typescriptClientsDir, rustClientsDir);

// Generate Rust client
contraWithdrawCodama.accept(
    renderRustVisitor(path.join(rustClientsDir, 'src', 'generated'), {
        formatCode: true,
        crateFolder: rustClientsDir,
        deleteFolderBeforeRendering: true,
    }),
);

// Generate TypeScript client
contraWithdrawCodama.accept(
    renderJavaScriptVisitor(path.join(typescriptClientsDir, 'src', 'generated'), {
        formatCode: true,
        deleteFolderBeforeRendering: true,
    }),
);

// Restore configuration files after generation
configPreserver.restore();
