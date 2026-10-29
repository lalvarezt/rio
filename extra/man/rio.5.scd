RIO(5)

# NAME

Rio - TOML configuration file format.

# SYNTAX

Rio's configuration file uses the TOML format. The format's specification
can be found at _https://toml.io/en/v1.0.0_.

# LOCATION

Rio doesn't create the config file for you, but it looks for one in the
following locations on UNIX systems:

. _$RIO_CONFIG_HOME/config.toml_
. _$XDG_CONFIG_HOME/rio/config.toml_
. _$HOME/.config/rio/config.toml_

On Windows, the config file will be looked for in:

. _%USERPROFILE%\\AppData\\Local\\rio\\config.toml_

# GENERAL

*theme* = _"<string>"_

	Theme name to use. When set, colors defined in the configuration file will be ignored.

	Default: _None_

*confirm-before-quit* = _true_ | _false_

	Require confirmation before quitting.

	Default: _true_

*confirm-before-close-tab* = _true_ | _false_

	Show confirmation before closing a tab or closing other tabs, including
	close buttons, keyboard shortcuts, and command palette actions. Cancel is
	focused by default. Closing the last tab closes its window after confirmation.
	Set to _false_ to close tabs immediately. Window quit confirmation is
	controlled separately by *confirm-before-quit*.

	Default: _true_

*draw-bold-text-with-light-colors* = _true_ | _false_

	When _true_, bold text is drawn using the bright color variants.

	Default: _false_

*env-vars* = [_"<string>"_,]

	Environment variables to set for spawned processes.

	Example:
		env-vars = [_"TERM=xterm-256color"_, _"COLORTERM=truecolor"_]

# WINDOW

This section documents the *[window]* table of the configuration file.

*width* = _<integer>_

	Window width in pixels.

	Default: _600_

*height* = _<integer>_

	Window height in pixels.

	Default: _400_

*mode* = _"Windowed"_ | _"Maximized"_ | _"Fullscreen"_

	Window startup mode.

	Default: _"Windowed"_

*opacity* = _<float>_

	Background opacity as a floating point number from _0.0_ to _1.0_.

	Default: _1.0_

*blur* = _true_ | _false_

	Request compositor to blur content behind transparent windows.

	Default: _false_

*decorations* = _"Enabled"_ | _"Disabled"_ | _"Transparent"_ | _"Buttonless"_

	Window decorations.

	*Enabled*
		Normal window decorations.
	*Disabled*
		No window decorations.
	*Transparent* _(macOS only)_
		Transparent title bar.
	*Buttonless* _(macOS only)_
		Title bar without buttons.

	Default: _"Enabled"_

*background-image* = { path = _"<string>"_, opacity = _<float>_ }

	Window background image. The image is uploaded once into a dedicated GPU
	texture and stretched to cover the full window.

	*path*
		Path to the background image file.
	*opacity*
		Image opacity from _0.0_ to _1.0_. Default _1.0_.

# FONTS

This section documents the *[fonts]* table of the configuration file.

*size* = _<float>_

	Font size in points.

	Default: _18.0_

*family* = _"<string>"_

	Font family name. When set, overrides all font variant families.

	Default: _"cascadiacode"_

*features* = [_"<string>"_,]

	OpenType font features to enable.

	Example:
		features = [_"ss02"_, _"ss03"_, _"liga"_]

*use-drawable-chars* = _true_ | _false_

	Use built-in drawable characters for box drawing.

	Default: _true_

*disable-warnings-not-found* = _true_ | _false_

	Disable warnings when fonts are not found.

	Default: _false_

*additional-dirs* = [_"<string>"_,]

	Additional directories to search for fonts.

*regular* = { family = _"<string>"_, style = _"<string>"_, width = _"<string>"_, weight = _<integer>_ }

	Regular font configuration.

	Default: { family = _"cascadiacode"_, style = _"Normal"_, width = _"Normal"_, weight = _400_ }

*bold* = { family = _"<string>"_, style = _"<string>"_, width = _"<string>"_, weight = _<integer>_ }

	Bold font configuration.

	Default: { family = _"cascadiacode"_, style = _"Normal"_, width = _"Normal"_, weight = _800_ }

*italic* = { family = _"<string>"_, style = _"<string>"_, width = _"<string>"_, weight = _<integer>_ }

	Italic font configuration.

	Default: { family = _"cascadiacode"_, style = _"Italic"_, width = _"Normal"_, weight = _400_ }

*bold-italic* = { family = _"<string>"_, style = _"<string>"_, width = _"<string>"_, weight = _<integer>_ }

	Bold italic font configuration.

	Default: { family = _"cascadiacode"_, style = _"Italic"_, width = _"Normal"_, weight = _800_ }

*extras* = [{ family = _"<string>"_ },]

	Additional fonts to load.

*emoji* = { family = _"<string>"_ }

	Emoji font configuration.

	Default: Built-in Twemoji font

*hinting* = _true_ | _false_

	Enable font hinting.

	Default: _true_

*symbol-map* = [{ chars = _"<string>"_, family = _"<string>"_ },]

	Map specific character ranges to fonts.

# COLORS

This section documents the *[colors]* table of the configuration file.

Colors are specified using their hexadecimal values with a _#_ prefix: _#RRGGBB_ or _#RRGGBBAA_.

*background* = _"<string>"_

	Default background color.

*foreground* = _"<string>"_

	Default foreground color.

*cursor* = _"<string>"_

	Cursor color.

*vi-cursor* = _"<string>"_

	Vi mode cursor color.

*black*, *red*, *green*, *yellow*, *blue*, *magenta*, *cyan*, *white* = _"<string>"_

	Standard ANSI colors.

*light-black*, *light-red*, *light-green*, *light-yellow*, *light-blue*, *light-magenta*, *light-cyan*, *light-white* = _"<string>"_

	Bright ANSI colors.

*dim-black*, *dim-red*, *dim-green*, *dim-yellow*, *dim-blue*, *dim-magenta*, *dim-cyan*, *dim-white* = _"<string>"_

	Dim ANSI colors.

*tabs* = _"<string>"_

	Inactive tab title color. Tab island backgrounds are derived from
	the window background color automatically.

*tabs-active* = _"<string>"_

	Active tab title color.

*split* = _"<string>"_

	Split separator color.

*search-match-background* = _"<string>"_

	Search match background color.

*search-match-foreground* = _"<string>"_

	Search match text color.

*search-focused-match-background* = _"<string>"_

	Focused search match background color.

*search-focused-match-foreground* = _"<string>"_

	Focused search match text color.

*selection-background* = _"<string>"_

	Selection background color.

*selection-foreground* = _"<string>"_

	Selection text color.

# CURSOR

This section documents the *[cursor]* table of the configuration file.

*shape* = _"block"_ | _"underline"_ | _"beam"_

	Cursor shape.

	Default: _"block"_

*blinking* = _true_ | _false_

	Enable cursor blinking.

	Default: _false_

*blinking-interval* = _<integer>_

	Cursor blinking interval in milliseconds. Range: 350-1200.

	Default: _800_

# SCROLL

This section documents the *[scroll]* table of the configuration file.

*multiplier* = _<float>_

	Scroll speed multiplier.

	Default: _3.0_

*divider* = _<float>_

	Scroll divider for fine-tuning.

	Default: _1.0_

# NAVIGATION

This section documents the *[navigation]* table of the configuration file.

*preset* = _"pill"_ | _"square"_

	Supply defaults for tab and split geometry. Explicit settings override the
	preset. Square tabs touch the upper-left edge, have no gaps, and keep close
	buttons visible. Square splits use rectangular corners and one-pixel borders.
	Dialogs, the command palette, search, tab customization, and hint tooltips
	also use square corners with this preset. Pill uses rounded corners.

	Default: _"pill"_

The optional *[navigation.tabs]* table overrides *height*, *margin*, *padding*,
*gap*, *radius*, *font-size*, *text-offset-y*, *icon-gap*, *close-button*,
*close-button-size*, *close-button-inset*, and *close-button-gap*.
Close-button modes are _"always"_, _"hover"_, and _"never"_. Geometry is in
logical pixels. Margins and padding accept arrays of one, two, or four values
in CSS order. Width and icon size remain configured with *min-tab-width*,
*max-tab-width*, and *tab-icon-size* in *[navigation]*.

Root *margin* controls the pane layout's distance from the window edge and
tabs. The existing *[panel]* table controls per-pane *margin*, *padding*,
*row-gap*, *column-gap*, *border-width*, and *border-radius*. These settings
also override preset defaults. Each pane has an outer margin, then its border,
then inner padding around the terminal cells. Border width is in physical
pixels; other pane geometry uses logical pixels. Geometry changes reload for
existing panes.
*divider-width* sets the horizontal and vertical split line width in logical
pixels (default: _1_). Set it to _0_ to hide the lines. Lines use the theme's
neutral border color. Spacing is measured from their center, so changing the
width does not move or resize panes. A single pane has no dividers.
Set *[panel].hide-if-single* to _true_ to hide a lone pane's frame and remove
only the border's inset. Root margin, pane margin, and padding still apply.
Default: _false_.

*hide-if-single* = _true_ | _false_

	Hide the tab strip when the window contains one tab. The default is _true_
	on macOS and _false_ on Linux and Windows. Tab shortcuts remain available.

*current-working-directory* = _true_ | _false_

	Open new tabs and splits in the focused pane's reported working directory.
	This also applies to SSH connections. Configure tab labels in *[title]*.

	Default: _true_

Tabs support only the _"pill"_ and _"square"_ presets. Remove *navigation.mode*,
*navigation.clickable*, *navigation.color-automation*, and
*navigation.use-terminal-title* from existing configurations. Replace the
former *tab-style* key with *preset* and its _"windows"_ value with _"square"_.
Removed navigation options produce a configuration error. The former
*window.macos-use-unified-titlebar* setting is no longer used.

# SHELL

This section documents the *[shell]* table of the configuration file.

*program* = _"<string>"_

	Shell program to execute.

	Default: System default shell

*args* = [_"<string>"_,]

	Arguments to pass to the shell.

# EDITOR

This section documents the *[editor]* table of the configuration file.

*program* = _"<string>"_

	Editor program for opening configuration files.

	Default: _"vi"_

*args* = [_"<string>"_,]

	Arguments to pass to the editor.

# BINDINGS

This section documents the *[bindings]* table of the configuration file.

*keys* = [{ key = _"<string>"_, with = _"<string>"_, action = _"<string>"_ },]

	Key bindings configuration.

	*key*
		Key name (e.g., _"q"_, _"Enter"_, _"F1"_).

	*with*
		Modifier keys: _"super"_, _"alt"_, _"ctrl"_, _"shift"_.
		Multiple modifiers can be combined with _|_.

	*action*
		Action to perform. Available actions include:
		_"Quit"_, _"CreateWindow"_, _"CloseWindow"_, _"Copy"_, _"Paste"_,
		_"ScrollUp"_, _"ScrollDown"_, _"ToggleViMode"_, _"OpenConfigEditor"_,
		and many others.

# RENDERER

This section documents the *[renderer]* table of the configuration file.

*performance* = _"High"_ | _"Low"_

	Rendering performance mode.

	Default: _"High"_

*backend* = _"Automatic"_ | _"GL"_ | _"Vulkan"_ | _"DX12"_ | _"Metal"_

	Graphics backend to use.

	Default: _"Automatic"_

*disable-renderer-when-unfocused* = _true_ | _false_

	Disable rendering when window is unfocused to save power.

	Default: _false_

*level* = _<integer>_

	Rendering level (0-3).

	Default: _3_

# DEVELOPER

This section documents the *[developer]* table of the configuration file.

*log-level* = _"OFF"_ | _"ERROR"_ | _"WARN"_ | _"INFO"_ | _"DEBUG"_ | _"TRACE"_

	Log level for debugging.

	Default: _"OFF"_

*enable-log-file* = _true_ | _false_

	Write logs to file.

	Default: _false_

*enable-fps-counter* = _true_ | _false_

	Display FPS counter.

	Default: _false_

# SEE ALSO

*rio*(1)

# BUGS

Found a bug? Please report it at _https://github.com/raphamorim/rio/issues_.

# MAINTAINERS

- Raphael Amorim <rapha850@gmail.com>
