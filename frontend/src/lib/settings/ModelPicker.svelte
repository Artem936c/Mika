<script lang="ts">
  // A model field with the provider's list under it: search, price, context,
  // abilities and the description of the model in focus.
  import { invoke } from '@tauri-apps/api/core';
  import Icon from '../Icon.svelte';
  import { isMessageKey, t } from '../i18n';
  import { byPrice, contextLabel, money, searchModels, shortName, type ModelInfo, type ModelList, type Purpose } from '../models';

  let {
    value = $bindable(''),
    purpose,
    baseUrl = '',
    kind = undefined,
    placeholder = '',
  }: {
    value: string;
    purpose: Purpose;
    /** The address in the form being edited; empty means the saved one. */
    baseUrl?: string;
    /** The LLM kind in the form being edited (only for the chat model). */
    kind?: string;
    placeholder?: string;
  } = $props();

  let open = $state(false);
  let loading = $state(false);
  let error = $state('');
  let list = $state<ModelList | null>(null);
  let query = $state('');
  let focus = $state<ModelInfo | null>(null);

  const found = $derived(list ? byPrice(searchModels(list.models, query)) : []);
  // Long lists render the cheapest part; the search narrows the rest.
  const shown = $derived(found.slice(0, 200));
  const countLine = $derived(
    [
      t('models.count', { count: found.length }),
      found.length > shown.length ? t('models.capped', { count: shown.length }) : '',
      t('models.cheap'),
      list && !list.filtered ? t('models.all') : '',
    ]
      .filter(Boolean)
      .join(' · '),
  );

  const label = (prefix: string, key: string) => {
    const k = `${prefix}.${key}`;
    return isMessageKey(k) ? t(k) : key;
  };

  function priceLine(m: ModelInfo): string {
    if (!list || m.prices.length === 0) return '';
    const p = m.prices[0];
    return `${money(p.value)}${list.currency ? ' ' + list.currency : ''} ${label('models.per', p.per)}`;
  }

  async function toggle() {
    open = !open;
    if (!open) return;
    error = '';
    loading = true;
    try {
      list = await invoke<ModelList>('list_models', { purpose, baseUrl: baseUrl || null, kind: kind ?? null });
      focus = list.models.find((m) => m.id === value) ?? null;
    } catch (e) {
      error = String(e);
      list = null;
    } finally {
      loading = false;
    }
  }

  function pick(m: ModelInfo) {
    value = m.id;
    open = false;
  }
</script>

<div class="mp">
  <div class="row">
    <input class="fld" bind:value {placeholder} spellcheck="false" />
    <button class="bt" class:on={open} onclick={toggle} title={t(open ? 'models.hide' : 'models.browse')}>
      <Icon name="search" size={14} />{t(open ? 'models.hide' : 'models.browse')}
    </button>
  </div>

  {#if open}
    <div class="panel">
      {#if loading}
        <p class="dim">{t('models.loading')}</p>
      {:else if error}
        <p class="bad">{error}</p>
      {:else if list}
        <input class="fld q" bind:value={query} placeholder={t('models.search')} spellcheck="false" />
        <p class="dim">{countLine}</p>
        <div class="cols">
          <ul>
            {#each shown as m (m.id)}
              <li>
                <button class="item" class:sel={focus?.id === m.id} class:cur={m.id === value} onclick={() => (focus = m)} ondblclick={() => pick(m)}>
                  <span class="nm">{shortName(m)}</span>
                  <span class="meta">
                    {#if m.prices.length}<span>{priceLine(m)}</span>{/if}
                    {#if m.context}<span>{contextLabel(m.context)}</span>{/if}
                    {#if m.tools}<span class="tag">{t('models.tools')}</span>{/if}
                  </span>
                </button>
              </li>
            {:else}
              <li class="dim">{t('models.empty')}</li>
            {/each}
          </ul>
          {#if focus}
            <div class="card">
              <b>{focus.name}</b>
              <code>{focus.id}</code>
              <div class="chips">
                {#if focus.context}<span>{t('models.context', { value: contextLabel(focus.context) })}</span>{/if}
                {#if focus.tools}<span>{t('models.tools')}</span>{/if}
                {#if focus.inputs.length}<span>{t('models.in')}: {focus.inputs.map((x) => label('models.mod', x)).join(', ')}</span>{/if}
                {#if focus.outputs.length}<span>{t('models.out')}: {focus.outputs.map((x) => label('models.mod', x)).join(', ')}</span>{/if}
              </div>
              {#each focus.prices as p (p.kind)}
                <div class="price">{label('models.price', p.kind)}: {money(p.value)}{list.currency ? ' ' + list.currency : ''} {label('models.per', p.per)}</div>
              {/each}
              {#if focus.description}<p class="desc">{focus.description}</p>{/if}
              <button class="bt pri" onclick={() => focus && pick(focus)} disabled={focus.id === value}>
                {focus.id === value ? t('models.current') : t('models.pick')}
              </button>
            </div>
          {/if}
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .row {
    display: flex;
    gap: 8px;
  }
  .row .fld {
    flex: 1;
    min-width: 0;
  }
  .row .bt {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .bt.on {
    border-color: rgba(143, 125, 255, 0.55);
  }
  .panel {
    margin-top: 8px;
    padding: 10px;
    border-radius: 14px;
    border: 1px solid var(--line);
    background: rgba(255, 255, 255, 0.025);
  }
  .q {
    width: 100%;
    box-sizing: border-box;
  }
  .dim {
    margin: 8px 2px;
    font-size: 12px;
    color: var(--dim);
  }
  .bad {
    margin: 4px 2px;
    font-size: 12.5px;
    color: var(--bad);
    user-select: text;
  }
  .cols {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: 10px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 260px;
    overflow-y: auto;
    display: grid;
    gap: 2px;
    align-content: start;
  }
  .item {
    all: unset;
    box-sizing: border-box;
    width: 100%;
    display: grid;
    gap: 2px;
    padding: 6px 9px;
    border-radius: 9px;
    cursor: pointer;
  }
  .item:hover {
    background: rgba(255, 255, 255, 0.05);
  }
  .item.sel {
    background: rgba(143, 125, 255, 0.16);
  }
  .item.cur .nm::after {
    content: ' ✓';
    color: var(--ok);
  }
  .nm {
    font-size: 13px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    font-size: 11px;
    color: var(--mute);
  }
  .tag {
    color: var(--c);
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
    max-height: 260px;
    overflow-y: auto;
    padding: 2px 4px;
    font-size: 12.5px;
  }
  .card b {
    font-size: 14px;
  }
  code {
    font-family: var(--mono);
    font-size: 11.5px;
    color: var(--mute);
    word-break: break-all;
    user-select: text;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
  }
  .chips span {
    padding: 2px 8px;
    border-radius: 9px;
    background: rgba(255, 255, 255, 0.06);
    font-size: 11.5px;
  }
  .price {
    color: var(--ink);
  }
  .desc {
    margin: 0;
    color: var(--mute);
    line-height: 1.45;
    user-select: text;
  }
  .card .bt {
    align-self: flex-start;
    margin-top: 4px;
  }
</style>
