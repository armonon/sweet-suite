# design-tokens

The **single source of truth** for the suite's look — colors (charcoal chrome + one accent), type scale, spacing, radius, and motion. Every app gets identical visuals because there is exactly one of these.

`tokens.toml` is plain data, deliberately **outside** the Cargo workspace. `platform/design` loads it and exposes typed tokens + the component kit (buttons, sliders, numeric drags, color pickers, menus, dialogs) built on top — "build it as a real component library, not per-app CSS" (docs/02 §3.2).

Change a token here → every app changes together. That is the point.
