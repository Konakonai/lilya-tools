package gui

import "github.com/egoist/mygo/ui"

// Architectural Catppuccin Lavender Light palette.
// Grounded in the host desktop's catppuccin-lavender-light.json with restrained
// contrast, hairline separation, and a cohesive desaturated chart spectrum.
var (
	ColBg             = ui.Hex("#faf8ff") // mSurface: calm lavender-tinted alabaster
	ColSurface        = ui.Hex("#ffffff") // quiet elevated plot/table surface
	ColSurfaceSubtle  = ui.Hex("#f3effc") // subtle hover / active tab tint
	ColHover          = ui.Hex("#eae4fa") // interactive hover
	ColHairline       = ui.Hex("#e6dff7") // crisp 1px architectural divider
	ColBorder         = ui.Hex("#d8d0f4") // control outline

	ColInk       = ui.Hex("#241d35") // primary text (high contrast)
	ColInkMuted  = ui.Hex("#665c80") // secondary text (WCAG AA >= 5.2:1 on #faf8ff)
	ColInkSubtle = ui.Hex("#887ea3") // quiet axis / tertiary metadata

	ColAccent        = ui.Hex("#7c5cfa") // single deliberate primary accent
	ColAccentHover   = ui.Hex("#6b49ec")
	ColAccentPressed = ui.Hex("#5a38d8")
	ColAccentSoft    = ui.Hex("#efeafd")

	// Semantic status accents (used sparingly inside data tables / alerts only).
	ColSuccess = ui.Hex("#1f8a52")
	ColWarning = ui.Hex("#b76e14")
	ColDanger  = ui.Hex("#d64550")
)

// SeriesPalette is a cohesive, desaturated mineral/botanical palette tailored to
// light lavender paper — avoids harsh neon saturation while preserving legibility.
var SeriesPalette = []ui.Color{
	ui.Hex("#7c5cfa"), // Lavender (primary)
	ui.Hex("#5274e5"), // Periwinkle Slate
	ui.Hex("#2c9684"), // Sage Teal
	ui.Hex("#d47a52"), // Muted Terracotta
	ui.Hex("#b362a6"), // Dusty Mauve
	ui.Hex("#6f8fa8"), // Steel Mist
	ui.Hex("#b0a6c7"), // Neutral Other
}

// HeatmapRamp maps 0..4 activity levels to Lavender paper ink intensities.
var HeatmapRamp = [5]ui.Color{
	ui.Hex("#f0ecfa"),
	ui.Hex("#dad1f8"),
	ui.Hex("#b7a3f5"),
	ui.Hex("#8f71f6"),
	ui.Hex("#6843e6"),
}

// LavenderLightTheme constructs the restrained Catppuccin Lavender Light theme.
func LavenderLightTheme() *ui.Theme {
	t := ui.LightTheme()
	t.Dark = false
	t.Background = ColBg
	t.Surface = ColSurface
	t.SurfaceHover = ColSurfaceSubtle
	t.SurfacePressed = ColHover
	t.Border = ColBorder
	t.Text = ColInk
	t.TextMuted = ColInkMuted
	t.Accent = ColAccent
	t.AccentHover = ColAccentHover
	t.AccentPressed = ColAccentPressed
	t.AccentText = ui.Hex("#ffffff")
	t.InactiveAccent = ui.Hex("#a594eb")
	t.Danger = ColDanger
	t.Warning = ColWarning
	t.Success = ColSuccess
	t.Selection = ColHover
	t.Focus = ColAccent
	t.Inverse = ColInk
	t.InverseText = ColBg
	t.Scrollbar = ui.Hex("#cbc1ea")
	t.Radius = 7
	t.Spacing = 4
	t.FontSize = 13
	return t
}

func seriesColor(idx int) ui.Color {
	if idx < 0 {
		return SeriesPalette[len(SeriesPalette)-1]
	}
	return SeriesPalette[idx%len(SeriesPalette)]
}
