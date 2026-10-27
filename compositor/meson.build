project('gnome-shell-plus', 'c',
  version: '0.1.0',
  meson_version: '>= 1.0',
  default_options: ['c_std=gnu11', 'warning_level=2'],
)

mutter_api = get_option('mutter_api')

deps = [
  dependency('libmutter-' + mutter_api),
  dependency('mutter-clutter-' + mutter_api),
  dependency('mutter-cogl-' + mutter_api),
  dependency('mutter-mtk-' + mutter_api),
  dependency('gio-unix-2.0'),
  dependency('gsettings-desktop-schemas'),
  dependency('cairo'),
  dependency('xkbregistry'),
]

add_project_arguments(
  '-Wno-unused-parameter',
  '-Wno-missing-field-initializers',
  '-DSHELLPLUS_PANEL_BINARY="@0@"'.format(get_option('panel_binary')),
  language: 'c',
)

executable('gnome-shell-plus',
  'src/main.c',
  'src/shellplus-plugin.c',
  'src/shellplus-animations.c',
  'src/shellplus-background.c',
  'src/shellplus-dbus.c',
  'src/shellplus-input-sources.c',
  'src/shellplus-panel-constraint.c',
  'src/shellplus-shell-dbus.c',
  'src/shellplus-switcher.c',
  dependencies: deps,
  install: true,
)
