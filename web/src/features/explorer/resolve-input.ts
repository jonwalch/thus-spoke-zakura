import type { Transaction, TxInput, TxOutput } from '@/lib/api';

export interface ResolvedTransparentInput {
  coinbase: boolean;
  prevTxid?: string;
  address?: string;
  valueZat?: number;
}

function spentOutput(input: TxInput, prev?: Transaction): TxOutput | undefined {
  if (input.vout === undefined || !prev) return undefined;
  return prev.vout.find((out) => out.n === input.vout) ?? prev.vout[input.vout];
}

/** Node vin is an outpoint. Address and value live on the spent output. */
export function resolveTransparentInput(
  input: TxInput,
  prev?: Transaction,
): ResolvedTransparentInput {
  if (input.coinbase) return { coinbase: true };

  const spent = spentOutput(input, prev);
  const address = input.scriptPubKey?.addresses[0] ?? spent?.scriptPubKey?.addresses[0];
  const valueZat = input.valueZat ?? spent?.valueZat;
  return {
    coinbase: false,
    ...(input.txid ? { prevTxid: input.txid } : {}),
    ...(address ? { address } : {}),
    ...(valueZat !== undefined ? { valueZat } : {}),
  };
}
