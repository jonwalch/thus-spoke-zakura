/**
 * Routes a query to the right view locally. The server's /search endpoint
 * guesses by probing the node, which costs a round trip and cannot deep-link;
 * the input's shape already tells us what it is.
 */
export function classifyQuery(raw: string): { path: string } | { error: string } {
  const query = raw.trim();
  if (!query) return { error: 'Enter a block height, block hash, transaction ID, or address.' };
  if (/^\d+$/.test(query)) return { path: `/explorer/block/${query}` };
  if (/^t[a-zA-Z0-9]{20,}$/.test(query)) return { path: `/explorer/address/${query}` };
  if (/^[0-9a-fA-F]{64}$/.test(query)) return { path: `/explorer/tx/${query.toLowerCase()}` };
  if (/^u(regtest)?1[a-z0-9]+$/i.test(query)) {
    return { error: 'Unified addresses are shielded and have no public chain history.' };
  }
  return { error: 'Not a recognised block height, hash, transaction ID, or transparent address.' };
}
