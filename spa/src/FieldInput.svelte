<script lang="ts">
  import type { FieldConfig, HojaRecord, TypeConfig } from './lib/api';
  import { recordLabel, titleOf } from './lib/views';

  let { name, field, value = $bindable(), config, records = [], disabled = false }: {
    name: string;
    field: FieldConfig;
    value: string | boolean;
    config: TypeConfig;
    records?: HojaRecord[];
    disabled?: boolean;
  } = $props();

  const id = `field-${Math.random().toString(36).slice(2, 10)}`;
  const choices = $derived(field.type === 'ref' ? records.filter(r => r.kind === field.ref_kind && !r.deleted) : []);
</script>

<div class="field" class:wide={field.type === 'text' || field.type === 'list'}>
  <label for={id}>
    {titleOf(name, field)}
    {#if field.required}<span class="required" title="Required">*</span>{/if}
    {#if field.person_only}<span class="badge person" title="Agents can neither write nor suggest this field">yours only</span>{/if}
  </label>
  {#if field.type === 'text'}
    <textarea {id} rows="4" bind:value {disabled}></textarea>
  {:else if field.type === 'tags' || field.type === 'list'}
    <textarea {id} rows="3" bind:value {disabled} placeholder={field.type === 'tags' ? 'one per line or comma-separated' : 'one per line'}></textarea>
  {:else if field.type === 'enum'}
    <select {id} bind:value {disabled}>
      <option value="">(none)</option>
      {#each field.options ?? [] as option (option)}<option value={option}>{titleOf(option)}</option>{/each}
    </select>
  {:else if field.type === 'ref'}
    <select {id} bind:value {disabled}>
      <option value="">(none)</option>
      {#each choices as record (record.id)}<option value={record.id}>{recordLabel(config, record)}</option>{/each}
    </select>
  {:else if field.type === 'bool'}
    <input {id} type="checkbox" checked={value === true} onchange={e => value = e.currentTarget.checked} {disabled} />
  {:else}
    <input {id} type="text" bind:value {disabled} placeholder={field.type === 'date' ? 'YYYY, YYYY-MM or YYYY-MM-DD' : ''} />
  {/if}
  {#if field.help}<small>{field.help}</small>{/if}
</div>
