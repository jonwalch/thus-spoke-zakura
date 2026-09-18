import { describe, expect, it } from 'vitest';
import type { Transaction, TxInput } from '@/lib/api';
import { resolveTransparentInput } from './resolve-input';

const prev: Transaction = {
  txid: 'ab'.repeat(32),
  vin: [],
  vout: [
    {
      n: 0,
      valueZat: 100_000_000,
      scriptPubKey: { addresses: ['tmSpentOutput'] },
    },
  ],
  vShieldedSpend: [],
  vShieldedOutput: [],
};

describe('resolveTransparentInput', () => {
  it('takes address and value from the spent output of the previous transaction', () => {
    const input: TxInput = { txid: prev.txid, vout: 0 };
    expect(resolveTransparentInput(input, prev)).toEqual({
      coinbase: false,
      prevTxid: prev.txid,
      address: 'tmSpentOutput',
      valueZat: 100_000_000,
    });
  });

  it('does not invent an amount for coinbase', () => {
    expect(resolveTransparentInput({ coinbase: '03' })).toEqual({ coinbase: true });
  });
});
