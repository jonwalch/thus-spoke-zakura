import { describe, expect, it, vi, afterEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import { MemoryRouter, Route, Routes } from 'react-router-dom';
import { renderWithProviders, requestUrl } from '@/test/utils';
import { TransactionDetail } from './TransactionDetail';

const TXID = 'd9'.repeat(32);
const PREV_TXID = 'ab'.repeat(32);
const INPUT_ADDRESS = 'tmAg2wTARgKBHA9mFouqR727cU98MJmfjSq';

function json(body: unknown): Promise<Response> {
  return Promise.resolve(
    new Response(JSON.stringify(body), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    }),
  );
}

afterEach(() => {
  vi.restoreAllMocks();
});

function renderTx(body: unknown, extras: Record<string, unknown> = {}) {
  vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
    const url = requestUrl(input);
    if (url.includes(`/transactions/${TXID}`)) return json(body);
    for (const [id, payload] of Object.entries(extras)) {
      if (url.includes(`/transactions/${id}`)) return json(payload);
    }
    return Promise.resolve(
      new Response(JSON.stringify({ error: { message: 'unexpected', status: 404 } }), {
        status: 404,
        headers: { 'content-type': 'application/json' },
      }),
    );
  });

  return renderWithProviders(
    <MemoryRouter initialEntries={[`/explorer/tx/${TXID}`]}>
      <Routes>
        <Route path="/explorer/tx/:txid" element={<TransactionDetail />} />
      </Routes>
    </MemoryRouter>,
  );
}

describe('TransactionDetail', () => {
  it('lists transparent inputs with address and value', async () => {
    renderTx({
      txid: TXID,
      vin: [
        {
          txid: PREV_TXID,
          vout: 0,
          valueZat: 100_000_000,
          scriptPubKey: { addresses: [INPUT_ADDRESS] },
        },
      ],
      vout: [
        {
          n: 0,
          valueZat: 90_000_000,
          scriptPubKey: { addresses: ['tmBnC1iW276Njs86Lfp7y7qwUit55wG5bDm'] },
        },
      ],
      vShieldedSpend: [],
      vShieldedOutput: [],
      orchard: { actions: [{}, {}] },
    });

    await waitFor(() => expect(screen.getByText('1 transparent input')).toBeInTheDocument());
    expect(screen.getByText(INPUT_ADDRESS)).toBeInTheDocument();
    expect(screen.getByText('1 ZEC')).toBeInTheDocument();
  });

  it('labels a coinbase input instead of inventing an address', async () => {
    renderTx({
      txid: TXID,
      vin: [{ coinbase: '03' }],
      vout: [{ n: 0, valueZat: 0, scriptPubKey: { addresses: [] } }],
      vShieldedSpend: [],
      vShieldedOutput: [],
    });

    await waitFor(() => expect(screen.getByText('1 transparent input')).toBeInTheDocument());
    expect(screen.getByText('Coinbase')).toBeInTheDocument();
  });

  it('loads address and value from the previous transaction when vin is only an outpoint', async () => {
    renderTx(
      {
        txid: TXID,
        vin: [{ txid: PREV_TXID, vout: 0 }],
        vout: [
          {
            n: 0,
            valueZat: 90_000_000,
            scriptPubKey: { addresses: ['tmBnC1iW276Njs86Lfp7y7qwUit55wG5bDm'] },
          },
        ],
        vShieldedSpend: [],
        vShieldedOutput: [],
      },
      {
        [PREV_TXID]: {
          txid: PREV_TXID,
          vin: [],
          vout: [
            {
              n: 0,
              valueZat: 100_000_000,
              scriptPubKey: { addresses: [INPUT_ADDRESS] },
            },
          ],
          vShieldedSpend: [],
          vShieldedOutput: [],
        },
      },
    );

    expect(await screen.findByText(INPUT_ADDRESS)).toBeInTheDocument();
    expect(screen.getByText('1 ZEC')).toBeInTheDocument();
  });
});
