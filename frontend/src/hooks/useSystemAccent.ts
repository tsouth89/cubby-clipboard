import { useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';

interface SystemAccentColor {
  red: number;
  green: number;
  blue: number;
  alpha: number;
}

function rgbToHsl(red: number, green: number, blue: number) {
  const r = red / 255;
  const g = green / 255;
  const b = blue / 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const lightness = (max + min) / 2;

  if (max === min) {
    return `0 0% ${Math.round(lightness * 1000) / 10}%`;
  }

  const delta = max - min;
  const saturation = lightness > 0.5 ? delta / (2 - max - min) : delta / (max + min);
  let hue =
    max === r
      ? (g - b) / delta + (g < b ? 6 : 0)
      : max === g
        ? (b - r) / delta + 2
        : (r - g) / delta + 4;
  hue /= 6;

  return `${Math.round(hue * 3600) / 10} ${Math.round(saturation * 1000) / 10}% ${Math.round(lightness * 1000) / 10}%`;
}

export function useSystemAccent() {
  useEffect(() => {
    const applyAccent = async () => {
      try {
        const color = await invoke<SystemAccentColor>('get_system_accent_color');
        const hsl = rgbToHsl(color.red, color.green, color.blue);
        document.documentElement.style.setProperty('--primary', hsl);
        document.documentElement.style.setProperty('--ring', hsl);
      } catch (error) {
        console.warn('Unable to read the Windows accent color:', error);
      }
    };

    void applyAccent();
    const unlisten = getCurrentWindow().onFocusChanged(({ payload: focused }) => {
      if (focused) void applyAccent();
    });

    return () => {
      void unlisten.then((stop) => stop());
    };
  }, []);
}
