import type { Config } from 'jest';

const config: Config = {
    preset: 'ts-jest/presets/default-esm',
    extensionsToTreatAsEsm: ['.ts'],
    testEnvironment: 'node',
    testMatch: ['**/*.test.ts'],
    testPathIgnorePatterns: ['/node_modules/', '/dist/'],
    coverageReporters: ['text', 'lcov', 'clover', 'json-summary'],
    moduleNameMapper: {
        '^(\\.{1,2}/.*)\\.js$': '$1',
    },
    transform: {
        '^.+\\.ts$': [
            'ts-jest',
            {
                useESM: true,
                tsconfig: {
                    module: 'es2022',
                },
            },
        ],
    },
};

export default config;
