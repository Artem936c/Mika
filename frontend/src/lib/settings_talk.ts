/**
 * A setting changed by conversation (decision 0046): the open settings page
 * reads its settings again. A change made on a page is already drawn there.
 */
export function changedByTalk(payload: unknown): boolean {
  if (typeof payload !== 'object' || payload === null) return false;
  const p = payload as { action?: unknown; by?: unknown };
  return p.action === 'settings_changed' && p.by === 'talk';
}
