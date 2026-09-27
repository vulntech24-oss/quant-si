---
name: Calm Quantitative Precision
colors:
  surface: '#f8f9fa'
  surface-dim: '#d9dadb'
  surface-bright: '#f8f9fa'
  surface-container-lowest: '#ffffff'
  surface-container-low: '#f3f4f5'
  surface-container: '#edeeef'
  surface-container-high: '#e7e8e9'
  surface-container-highest: '#e1e3e4'
  on-surface: '#191c1d'
  on-surface-variant: '#45464d'
  inverse-surface: '#2e3132'
  inverse-on-surface: '#f0f1f2'
  outline: '#76777d'
  outline-variant: '#c6c6cd'
  surface-tint: '#565e74'
  primary: '#000000'
  on-primary: '#ffffff'
  primary-container: '#131b2e'
  on-primary-container: '#7c839b'
  inverse-primary: '#bec6e0'
  secondary: '#006c4a'
  on-secondary: '#ffffff'
  secondary-container: '#82f5c1'
  on-secondary-container: '#00714e'
  tertiary: '#000000'
  on-tertiary: '#ffffff'
  tertiary-container: '#410002'
  on-tertiary-container: '#f63a35'
  error: '#ba1a1a'
  on-error: '#ffffff'
  error-container: '#ffdad6'
  on-error-container: '#93000a'
  primary-fixed: '#dae2fd'
  primary-fixed-dim: '#bec6e0'
  on-primary-fixed: '#131b2e'
  on-primary-fixed-variant: '#3f465c'
  secondary-fixed: '#85f8c4'
  secondary-fixed-dim: '#68dba9'
  on-secondary-fixed: '#002114'
  on-secondary-fixed-variant: '#005137'
  tertiary-fixed: '#ffdad6'
  tertiary-fixed-dim: '#ffb4ab'
  on-tertiary-fixed: '#410002'
  on-tertiary-fixed-variant: '#93000b'
  background: '#f8f9fa'
  on-background: '#191c1d'
  surface-variant: '#e1e3e4'
typography:
  display-lg:
    fontFamily: Geist
    fontSize: 36px
    fontWeight: '600'
    lineHeight: 44px
    letterSpacing: -0.03em
  display-lg-mobile:
    fontFamily: Geist
    fontSize: 28px
    fontWeight: '600'
    lineHeight: 36px
    letterSpacing: -0.02em
  headline-lg:
    fontFamily: Geist
    fontSize: 24px
    fontWeight: '600'
    lineHeight: 32px
    letterSpacing: -0.02em
  headline-md:
    fontFamily: Geist
    fontSize: 20px
    fontWeight: '500'
    lineHeight: 28px
    letterSpacing: -0.015em
  headline-sm:
    fontFamily: Geist
    fontSize: 16px
    fontWeight: '500'
    lineHeight: 24px
    letterSpacing: -0.01em
  body-lg:
    fontFamily: Geist
    fontSize: 15px
    fontWeight: '400'
    lineHeight: 24px
    letterSpacing: -0.005em
  body-md:
    fontFamily: Geist
    fontSize: 14px
    fontWeight: '400'
    lineHeight: 22px
    letterSpacing: 0em
  body-sm:
    fontFamily: Geist
    fontSize: 13px
    fontWeight: '400'
    lineHeight: 18px
    letterSpacing: 0em
  label-md:
    fontFamily: Geist
    fontSize: 12px
    fontWeight: '500'
    lineHeight: 16px
    letterSpacing: 0.02em
  label-sm:
    fontFamily: Geist
    fontSize: 11px
    fontWeight: '500'
    lineHeight: 14px
    letterSpacing: 0.04em
  mono-data-lg:
    fontFamily: JetBrains Mono
    fontSize: 18px
    fontWeight: '500'
    lineHeight: 24px
    letterSpacing: -0.02em
  mono-data-md:
    fontFamily: JetBrains Mono
    fontSize: 13px
    fontWeight: '400'
    lineHeight: 18px
    letterSpacing: -0.01em
  mono-data-sm:
    fontFamily: JetBrains Mono
    fontSize: 11px
    fontWeight: '400'
    lineHeight: 16px
    letterSpacing: 0em
rounded:
  sm: 0.125rem
  DEFAULT: 0.25rem
  md: 0.375rem
  lg: 0.5rem
  xl: 0.75rem
  full: 9999px
spacing:
  gutter: 1rem
  gutter-lg: 1.5rem
  margin: 1rem
  margin-md: 1.5rem
  margin-lg: 2rem
  space-xs: 0.25rem
  space-sm: 0.5rem
  space-md: 0.75rem
  space-lg: 1rem
  space-xl: 1.5rem
  space-2xl: 2rem
---

## Brand & Style

This design system embodies the calculated serenity of high-conviction decision-making. Built for quantitative researchers, portfolio managers, and sophisticated allocators, the aesthetic strips away the manic, neon-saturated clutter typical of legacy financial terminals. It replaces information overload with spatial composure, intellectual clarity, and rigorous visual hierarchy.

The design philosophy unites **Swiss Modernism** and **Tactile Precision Engineering**:
- **Radical Restraint:** Density is achieved through typographic micro-hierarchy rather than graphical clutter. Progressive disclosure isolates cognitive noise.
- **Architectural Anchor:** Four-corner layout anchors provide stable navigational boundaries, keeping the analytical viewport undisturbed.
- **Instrument Quality:** Data displays mimic high-grade technical drafting sheets—featuring ultra-subtle micro-dot canvas structures, razor-thin 1px borders, and pure white active modules on warm mineral backdrops.

## Colors

The palette is anchored by low-fatigue off-whites and deep slate charcoals, treating functional color as an alert mechanism rather than decoration.

### Canvas & Surface Structure
- **Base Canvas:** `#F8F9FA` to `#FBFBFA` provides an organic, glare-free working background.
- **Card Surfaces:** `#FFFFFF` pure white, creating soft optical separation above the textured base.
- **Borders & Dividers:** Crisp 1px `#E2E8F0` and `#E5E7EB`. Never double up borders against card edges.

### Typography & Content
- **Primary Ink:** `#0F172A` deep charcoal for primary values, metrics, and display text. Avoid pure `#000000` to prevent harsh visual tension against light cards.
- **Secondary Slate:** `#64748B` for analytical descriptors, table labels, and structural metadata.
- **Tertiary Muted:** `#94A3B8` for timestamps, inactive states, and subtle dot grid patterns.

### Semantic Performance Indicators
Market directional colors are purposefully restrained to prevent emotional trading:
- **Positive / Gain / Long:** `#059669` (Dark Forest Emerald) for text and indicators, paired with `#ECFDF5` for fill pills.
- **Negative / Loss / Short:** `#DC2626` (Muted Vermilion) for text and indicators, paired with `#FEF2F2` for fill pills.
- **Neutral / Volatility Hold:** `#64748B` with `#F1F5F9` container fills.

## Typography

The type system prioritizes mathematical scanning, numerical alignment, and typographic hierarchy. 

- **Primary Interface Typeface (Geist):** Clean, geometric neo-grotesque contours with tight letter spacing for executive summaries, headers, and UI controls.
- **Quantitative Data Typeface (JetBrains Mono):** Utilized strictly for tabular data, currency values, standard deviations, basis points, and model equations. Monospaced tabular figures guarantee column stability across live-streaming financial payloads.
- **Optical Rules:** All tabular figures must feature proportional lining disabled (`font-variant-numeric: tabular-nums`). Headers at `20px` and above require subtle negative tracking (`-0.015em` to `-0.03em`) to reinforce an authoritative, editorial finish.

## Layout & Spacing

The canvas is driven by an asymmetric 12-column modular grid flanked by a **Four-Corner Layout System**:
- **Top-Left:** Workspace identity & strategy context switcher.
- **Top-Right:** Global system state, live telemetry latency indicator, and profile action.
- **Bottom-Left:** Timeframe horizon toggles (1D, 1W, 1M, YTD, Max) and continuous playback timeline.
- **Bottom-Right:** Active terminal command prompt (`⌘K`) and export/run action hub.

### Canvas Grid & Texture
The global background features an SVG micro-dot matrix pattern (`1px` dots, spaced `24px` apart, color `#E2E8F0` at 60% opacity) that remains fixed relative to the viewport. It visually enforces the metric drafting table feel without disturbing reading flow.

### Responsive Breakpoints
- **Desktop (≥ 1280px):** 12 columns, 24px gutters, fixed four-corner perimeter pinned at `margin-lg` (32px).
- **Tablet (768px – 1279px):** 8 columns, 16px gutters, bottom corner clusters consolidate into a pinned bottom utility bar.
- **Mobile (< 768px):** 4 columns, 12px gutters, corners fold into a top bar and a minimal bottom-sheet navigation dock.

## Elevation & Depth

Spatial depth relies on crisp boundaries and hairline structural borders instead of heavy, distracting drop shadows.

1. **Surface 0 (Base Canvas):** Textured off-white `#F8F9FA`. Recessed areas, structural margins, and empty states sit directly on this layer.
2. **Surface 1 (Card Modules & Data Grids):** Pure `#FFFFFF` surface enclosed by a continuous 1px solid `#E2E8F0` hairline border. Shadow is microscopic: `0 1px 2px rgba(15, 23, 42, 0.03)`.
3. **Surface 2 (Interactive Flyouts & Overlays):** `#FFFFFF` surfaces with a 1px `#CBD5E1` border and a soft ambient drop: `0 4px 12px -2px rgba(15, 23, 42, 0.05), 0 2px 4px -1px rgba(15, 23, 42, 0.02)`.
4. **Surface 3 (Command Palette & System Modals):** Layered above a diffused backdrop blur (`backdrop-filter: blur(8px); background-color: rgba(248, 249, 250, 0.8)`). Elevation is bounded by a high-definition edge: `0 12px 32px -4px rgba(15, 23, 42, 0.08)`.

## Shapes

The geometric silhouette is sharp, industrial, and calibrated. Corners are intentionally restrained (`6px` to `8px`) to evoke physical lab instrumentation and fine-machined hardware.

- **Base Radius (Buttons, Inputs, Metric Badges):** `6px` (`0.375rem`).
- **Container Radius (Data Cards, Analytical Panels, Viewports):** `8px` (`0.5rem`).
- **Max Radius (Modals, Overlays):** `10px` (`0.625rem`).
- **Pills / Status Dots:** Circular (`9999px`) reserved exclusively for live pipeline indicators and micro delta-status badges.

## Components

### Buttons
- **Primary:** `#0F172A` deep slate background, `#FFFFFF` text, `6px` radius, height `34px`. Subtle transition on hover to `#1E293B`. Zero spread shadow.
- **Secondary / Sub-action:** `#FFFFFF` background, `1px solid #E2E8F0`, `#0F172A` text. Hover brings `#F8F9FA` fill and `#CBD5E1` border tint.
- **Ghost Utility:** Borderless, `#64748B` text, shifts to `#0F172A` on `#F1F5F9` hover background.

### Input Fields & Selectors
- Compact `34px` height with `10px` horizontal padding.
- Crisp `1px solid #E2E8F0` border on `#FFFFFF` ground. Focused state transitions smoothly to `1px solid #0F172A` with an inner halo: `box-shadow: 0 0 0 1px #0F172A`. Never use thick rings or loud saturated blues.
- Monospaced typography for parameters, tickers, alpha expressions, and date-range inputs.

### Data Cards & Modules
- Minimal header zone (`40px` height) with a `1px` bottom border line `#F1F5F9`.
- Header houses the metric descriptor (`label-sm`), the active timeframe, and a ghost ellipsis for progressive expansion.
- Main value presented in high-contrast `Geist` or `JetBrains Mono` with muted trend pill adjacent.

### Quantitative Delta Badges (Chips)
- Height `20px`, horizontal padding `6px`, `4px` radius.
- **Long/Up:** Background `#ECFDF5`, text `#059669`, arrow icon `10px`.
- **Short/Down:** Background `#FEF2F2`, text `#DC2626`, arrow icon `10px`.
- Typography set to `mono-data-sm` with explicit sign indicator (`+` or `−`).

### Data Tables & Financial Matrices
- Row height fixed at `36px` for dense scanning without touching text boundaries.
- Alternating rows remain uncolored; separation is achieved via `1px solid #F1F5F9` row dividers.
- Hovering reveals a complete row highlight in `#F8F9FA`.
- Numerical columns right-align; text and ticker columns left-align.

### Progressive Disclosure Drawers
- Deep analysis (factor decomposition, covariance matrices, backtest logs) expands laterally via sliding side sheets rather than intrusive modal windows, preserving situational awareness of the primary desk.