const path = require('path');
const { CleanWebpackPlugin } = require('clean-webpack-plugin');

module.exports = {
  mode: 'production',
  entry: {
    'send-transaction': './src/send-transaction.ts',
    'max-throughput': './src/max-throughput.ts',
  },
  output: {
    path: path.join(__dirname, 'dist'),
    libraryTarget: 'commonjs',
    filename: '[name].js',
  },
  resolve: {
    extensions: ['.ts', '.js'],
  },
  module: {
    rules: [
      {
        test: /\.ts$/,
        use: 'ts-loader',
        exclude: /node_modules/,
      },
    ],
  },
  target: 'web',
  externals: /^k6(\/.*)?$/,
  plugins: [
    new CleanWebpackPlugin(),
  ],
  optimization: {
    minimize: false,
  },
};