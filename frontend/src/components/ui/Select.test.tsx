import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { Select } from './Select';

const options = [
  { label: 'English', value: 'en' },
  { label: 'Spanish', value: 'es' },
];

afterEach(cleanup);

describe('Select', () => {
  it('uses native select semantics and reports changes', () => {
    const onChange = vi.fn();
    render(<Select ariaLabel="Language" value="en" onChange={onChange} options={options} />);
    const select = screen.getByRole('combobox', { name: 'Language' });

    expect(select).toHaveValue('en');
    expect(screen.getByRole('option', { name: 'English' })).toHaveProperty('selected', true);
    fireEvent.change(select, { target: { value: 'es' } });

    expect(onChange).toHaveBeenCalledWith('es');
  });

  it('shows a disabled placeholder when no value is selected', () => {
    render(
      <Select
        ariaLabel="Language"
        value=""
        onChange={vi.fn()}
        options={options}
        placeholder="Language"
      />
    );

    expect(screen.getByRole('combobox')).toHaveValue('');
    expect(screen.getByRole('option', { name: 'Language' })).toBeDisabled();
  });

  it('honors the disabled state', () => {
    render(
      <Select ariaLabel="Language" value="en" onChange={vi.fn()} options={options} disabled />
    );

    expect(screen.getByRole('combobox')).toBeDisabled();
  });
});
