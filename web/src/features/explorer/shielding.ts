import type { Transaction } from '@/lib/api';

export interface ShieldingSummary {
  orchardActions: number;
  transparentInputs: number;
  transparentOutputs: number;
  saplingSpends: number;
  saplingOutputs: number;
  /** True when nothing about the transfer is visible on-chain. */
  fullyShielded: boolean;
  /** True when value crosses the shielded/transparent boundary. */
  mixed: boolean;
}

/**
 * Summarises what a transaction actually reveals publicly. An Orchard-only
 * transfer has no transparent inputs or outputs and a zero value balance, so
 * the amount, sender and recipient are all absent from chain data — the
 * property the dashboard claims but never previously demonstrated.
 */
export function summariseShielding(tx: Transaction): ShieldingSummary {
  const orchardActions = tx.orchard?.actions.length ?? 0;
  const transparentInputs = tx.vin.length;
  const transparentOutputs = tx.vout.length;
  const saplingSpends = tx.vShieldedSpend.length;
  const saplingOutputs = tx.vShieldedOutput.length;

  const shieldedPresent = orchardActions > 0 || saplingSpends > 0 || saplingOutputs > 0;
  const transparentPresent = transparentInputs > 0 || transparentOutputs > 0;

  return {
    orchardActions,
    transparentInputs,
    transparentOutputs,
    saplingSpends,
    saplingOutputs,
    fullyShielded: shieldedPresent && !transparentPresent,
    mixed: shieldedPresent && transparentPresent,
  };
}
