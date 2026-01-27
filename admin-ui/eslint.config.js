import js from '@eslint/js'
import globals from 'globals'
import reactHooks from 'eslint-plugin-react-hooks'
import reactRefresh from 'eslint-plugin-react-refresh'
import tseslint from 'typescript-eslint'
import { defineConfig, globalIgnores } from 'eslint/config'

export default defineConfig([
  globalIgnores(['dist', '.vite']),
  {
    files: ['**/*.{ts,tsx}'],
    extends: [
      js.configs.recommended,
      tseslint.configs.recommended,
      reactHooks.configs.flat.recommended,
      reactRefresh.configs.vite,
    ],
    languageOptions: {
      ecmaVersion: 2020,
      globals: globals.browser,
    },
    rules: {
      'no-restricted-syntax': [
        'error',
        {
          selector: 'VariableDeclarator[init.callee.name="useWallet"] Property[key.name="account"]',
          message: 'Do not destructure "account" from useWallet(). Use "wallet" instead and access wallet.accounts[0].',
        },
        {
          selector: 'VariableDeclarator[init.callee.name="useWallet"] Property[key.name="address"]',
          message: 'Do not destructure "address" from useWallet(). Use "wallet" instead.',
        },
      ],
    },
  },
])
