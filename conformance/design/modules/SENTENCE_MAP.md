# design.md §4 sentence-to-cell map (lines 562-984)

One row per sentence of design.md §4 Modules and packages, as of 2026-11-24, naming the cell in this directory (or the test elsewhere) that covers it. NEW marks a cell added by this audit; GAP and FAILS mark what is not covered.

| # | Section | Sentence | Cell(s) |
|---|---|---|---|
| s4.1 | Modules | **The directory tree is the module tree.** Each `.kara` file under `src/` is a module, named by its path from `src/`: `src/db/connection.kar | module_tree_and_import_forms |
| s4.2 | Modules | There is no module declaration. | module_tree_and_import_forms (no declaration in any cell) |
| s4.3 | Modules | - **Entry points.** `main.kara` for an executable, `lib.kara` for a library. | init_bin, init_lib, path_dependencies_and_hyphen_names (lib.kara) |
| s4.4 | Modules | A package cannot have both. | both_entry_points_refused |
| s4.5 | Modules | Items in `main.kara` or `lib.kara` belong to the package root: `fn start()` in `main.kara` is `start`, not `main.start`. | NEW root_item_not_main_prefixed_refused + root_item_imported_by_name |
| s4.6 | Modules | - **One file per module path.** `src/db.kara` defines `db`. | NEW module_file_beside_its_directory; prelude_name_shadowed_by_import (src/out.kara) |
| s4.7 | Modules | `src/db/mod.kara` is not recognized. | mod_kara_not_recognized_refused |
| s4.8 | Modules | To put items directly in `db`, write them in `src/db.kara` or re-export them there. | NEW module_file_beside_its_directory |
| s4.9 | Modules | - **Fully qualified paths.** Every type, function and effect resource is named by its module path. | module_tree_and_import_forms |
| s4.10 | Modules | `db.UserDB` in one package and `otherlib.db.UserDB` from a dependency are distinct resources, and the effect checker treats them as non-conf | GAP: not covered (needs two packages' same-named effect resources in a `par`); left open |
| s4.11 | Modules | **The package name in paths.** | rationale or cross-reference, no cell |
| s4.12 | Modules | - **Inside a package**, paths omit the package name. | package_name_not_in_paths_refused |
| s4.13 | Modules | In package `myproject`, `src/db/connection.kara` is `db.connection`, and its items are imported as `db.connection.Connection`, never `myproj | package_name_not_in_paths_refused, module_tree_and_import_forms |
| s4.14 | Modules | - **Across packages**, a path starts with the dependency's name. | path_dependencies_and_hyphen_names |
| s4.15 | Modules | If `webserver` depends on `http`, it writes `import http.client.Connection` (from `http`'s `client` module) or `import http.Connection` (an  | path_dependencies_and_hyphen_names (`logx.log` root item, `logx.utils.x.u` module item) |
| s4.16 | Modules | The resolver recognizes a dependency by matching the first segment against the dependencies' import names: each one's `alias`, else the `[pa | dependency_declared_import_name, dependency_alias_overrides_import_name, path_dependencies_and_hyphen_names |
| s4.17 | Modules | - **A local module hides a dependency of the same name.** If a package has `src/db.kara` and a dependency named `db`, `db` means the local m | local_module_hides_dependency |
| s4.18 | Modules | To reach the dependency, give it an alias in `kara.toml`: | local_module_hides_dependency (alias `logx_ext`) |
| s4.19 | Modules | ```toml [dependencies] db = { version = "1.0", alias = "db_ext" } ``` | local_module_hides_dependency (alias `logx_ext`) |
| s4.20 | Modules | Then `import db_ext.Connection` names the dependency. | local_module_hides_dependency (alias `logx_ext`) |
| s4.21 | Modules | - **Definitions know their package.** A dependency's modules are rooted at the dependency's name, so two dependencies that both have a `util | path_dependencies_and_hyphen_names (two `utils` stay distinct) |
| s4.22 | Modules | Items of the package being compiled carry no package segment. | package_name_not_in_paths_refused |
| s4.23 | Modules | **No circular module dependencies.** The compiler builds a module graph from each file's `import` statements (including `pub import` edges)  | module_cycle_refused; NEW module_cycle_through_pub_import_refused (pub import edge, suggestion text), control module_diamond_is_not_a_cycle |
| s4.24 | Modules | **Colocated tests.** A test file has the `_test.kara` suffix and sits beside the code it tests. | test_file_sees_private_items |
| s4.25 | Modules | `src/db/connection_test.kara` tests `db.connection` and can see its `private` items. | test_file_sees_private_items |
| s4.26 | Modules | `karac build` skips `_test.kara` files; `karac test` includes them and adds the test prelude ([§14](#14-testing)). | NEW build_skips_test_files; test_file_sees_private_items (test includes them) |
| s4.27 | Modules | `karac check` checks them too, with the test prelude, so a package whose tests do not compile is not reported clean (ruling 2026-10-30). | NEW check_checks_test_files_refused |
| s4.28 | Modules | There is no separate `tests/` root. | statement of absence, no cell |
| s4.29 | Imports | Every reference to another module's item is brought into scope by an `import` at the top of the file. | NEW import_required_for_another_module_refused |
| s4.30 | Imports | Paths are absolute: from the current package's root, or from a dependency's name. | module_tree_and_import_forms |
| s4.31 | Imports | There is no `self.`, `super.` or relative form. | NEW relative_import_forms_refused |
| s4.32 | Imports | **Binding rule.** The last segment of each path is bound in the current scope: `import a.b.c;` binds `c`, `import a.b.{c, d};` binds `c` and | module_tree_and_import_forms (`as`, group, nested group, module binding) |
| s4.33 | Imports | Modules and items follow the same rule: `import db.connection;` binds the module `connection`, so its items are `connection.Connection`. | module_tree_and_import_forms (`as`, group, nested group, module binding) |
| s4.34 | Imports | **Groups and wildcards expand first.** `import a.{b.{c, d}, e};` is `import a.b.c; import a.b.d; import a.e;`. | module_tree_and_import_forms (`as`, group, nested group, module binding) |
| s4.35 | Imports | A wildcard expands to the single-item imports it stands for, so the forms compose (`import a.{b.*, c};`) and later phases see only single it | NEW wildcard_composes_in_a_group |
| s4.36 | Imports | `*` must be the last segment; `import path.* as X;` is not a form. | NEW wildcard_with_as_refused |
| s4.37 | Imports | **Wildcards** bring in exactly the items a single-item import of that module could bind ([Visibility](#visibility)). | prelude_name_shadowed_by_wildcard, dependency_default_items_not_importable_refused |
| s4.38 | Imports | Submodules are not items; import them by path. | NEW wildcard_skips_submodules_refused |
| s4.39 | Imports | - An explicit import beats a wildcard. | module_tree_and_import_forms (`which` explicit over wildcard) |
| s4.40 | Imports | With `import http.Response;` and `import net.*;`, `Response` is `http.Response`. | module_tree_and_import_forms |
| s4.41 | Imports | - Two wildcards that both supply a name are not an error at the import. | NEW two_wildcards_unused_clash_accepted |
| s4.42 | Imports | The name is ambiguous, and using it is `E0124 AmbiguousWildcardImport`; an explicit import resolves it. | wildcard_name_from_two_wildcards_refused; NEW two_wildcards_explicit_import_resolves |
| s4.43 | Imports | Other names from both wildcards work normally. | module_tree_and_import_forms (only_net), NEW two_wildcards_unused_clash_accepted |
| s4.44 | Imports | - Prelude names have the lowest priority: any import shadows them. | prelude_name_shadowed_by_import, prelude_name_shadowed_by_wildcard |
| s4.45 | Imports | **Re-exports.** `pub import` re-exports an item from the current module. | reexport_keeps_identity |
| s4.46 | Imports | Users see it at the shorter path; the identity is unchanged, so `mylib.Connection` and `mylib.db.connection.Connection` are the same type. | reexport_keeps_identity |
| s4.47 | Imports | The original must be visible to the re-exporting module, so `pub import` of a `private` item from outside its directory is a compile error. | reexport_of_private_item_refused |
| s4.48 | Imports | **Resolution.** For `import a.b.c.Item;` in module `R`: 1. | header |
| s4.49 | Imports | Start at the package root (or at the dependency named by the first segment). 2. | path_dependencies_and_hyphen_names |
| s4.50 | Imports | Descend one child module per segment except the last. | module_tree_and_import_forms |
| s4.51 | Imports | A miss is `E0112 UnknownModule`, with "did you mean" suggestions. 3. | unknown_module_suggests_refused |
| s4.52 | Imports | Look up the last segment among the target module's items. | module_tree_and_import_forms |
| s4.53 | Imports | A miss is `E0113 UnknownItemInModule`, with suggestions and, for a module with at most 10 exports, the list of exports. | unknown_item_lists_exports_refused |
| s4.54 | Imports | An item found but not visible from `R` is `E0111 PrivateItemAccess`. 4. | private_from_other_directory_refused |
| s4.55 | Imports | Bind the name in `R`. | module_tree_and_import_forms |
| s4.56 | Imports | **The prelude.** Every file implicitly starts with `import std.prelude.*;`. | prelude_name_shadowed_by_import |
| s4.57 | Imports | It goes through the normal import machinery at the lowest precedence, so any import or local name shadows a prelude name; a lint flags the l | prelude_name_shadowed_by_import/_wildcard; NEW local_fn_shadows_prelude (local name); lint part: not covered |
| s4.58 | Imports | - **Primitive types** are built in and need no import. | every cell (no import of primitives) |
| s4.59 | Imports | - **Standard types, traits and functions** in the prelude are listed in [library/README.md](library/README.md). | rationale or cross-reference, no cell |
| s4.60 | Imports | The prelude holds the core types and traits; everything else is under `std.*`. | cross-reference to library/README.md, no cell |
| s4.61 | Imports | - **Compiler builtins** (`todo`, `unreachable`, `panic`, `dbg`, `assert`, `assert_eq`, `assert_ne`) are prelude functions, usable in every f | NEW builtins_in_production_code |
| s4.62 | Imports | They are declared in standard-library source under `#[compiler_builtin]`, so documentation and editors see them. | implementation note, no cell |
| s4.63 | Imports | The compiler implements them, because they need the `Never` type, source locations and release-mode elision. | rationale or cross-reference, no cell |
| s4.64 | Imports | - `_test.kara` files also get the test prelude ([§14](#14-testing)). | test_file_sees_private_items (§14 cells cover the test prelude) |
| s4.65 | Visibility | / Keyword / Visible to / | table header |
| s4.66 | Visibility | / `pub` / Users of the package, and every file in it / | path_dependencies_and_hyphen_names (users), module_tree_and_import_forms |
| s4.67 | Visibility | / *(none)* / Every file in the package, not its users / | private_same_directory_default_package; dependency_default_items_not_importable_refused (not users) |
| s4.68 | Visibility | / `private` / Files in the same directory only / | private_same_directory_default_package, private_from_other_directory_refused |
| s4.69 | Visibility | - **`pub`** marks the public API. | non_pub_type_in_pub_signature_refused |
| s4.70 | Visibility | Public functions declare their effects ([§12](#12-effects)). | §12 effects cells (tests/effects_conformance.rs) |
| s4.71 | Visibility | - **The default** is package-internal: most code is internal helpers that other files in the package call. | private_same_directory_default_package |
| s4.72 | Visibility | Private functions' effects are inferred. | §12 effects cells |
| s4.73 | Visibility | - **`private`** limits an item to its directory, for helpers shared by related files (`db/connection.kara` and `db/schema.kara` sharing `db/ | private_same_directory_default_package |
| s4.74 | Visibility | It does not extend to parent or child directories. | private_from_child_directory_refused; NEW private_from_parent_directory_refused |
| s4.75 | Visibility | **Enforcement.** For a lookup of item `X`, defined in module `D`, from module `R`: | header |
| s4.76 | Visibility | / `X` is / Allowed when / | path_dependencies_and_hyphen_names, private_same_directory_default_package, private_from_other_directory_refused |
| s4.77 | Visibility | / `pub` / always / | path_dependencies_and_hyphen_names, private_same_directory_default_package, private_from_other_directory_refused |
| s4.78 | Visibility | / default / `R` and `D` are in the same package / | path_dependencies_and_hyphen_names, private_same_directory_default_package, private_from_other_directory_refused |
| s4.79 | Visibility | / `private` / `R` and `D` are in the same directory. | path_dependencies_and_hyphen_names, private_same_directory_default_package, private_from_other_directory_refused |
| s4.80 | Visibility | Entry files (`main.kara`, `lib.kara`) count as in `src/`, and a `_test.kara` file shares the directory of the module it tests. / | private_entry_file_counts_as_src, test_file_sees_private_items |
| s4.81 | Visibility | A non-`pub` type in a `pub` signature is `E0221 PrivateTypeInPublicSignature`. | non_pub_type_in_pub_signature_refused |
| s4.82 | Visibility | Direct access to an invisible item is `E0111 PrivateItemAccess`. | private_from_other_directory_refused |
| s4.83 | Visibility | **Why the default is package-internal.** In languages where the default is private to the module (Java, C#), modules are files or classes. | rationale or cross-reference, no cell |
| s4.84 | Visibility | In Kāra, modules are directories, and a directory-private default would force a keyword onto nearly every call between directories. | rationale or cross-reference, no cell |
| s4.85 | Visibility | Annotating only the boundaries (`pub` for the API, `private` for directory helpers) covers the common case. | rationale or cross-reference, no cell |
| s4.86 | Visibility | Directories otherwise only organize names: a default-visibility function in `db/connection.kara` is visible to `ui/render.kara`. | private_same_directory_default_package (ui/render.kara) |
| s4.87 | Visibility | **Struct fields** use the same three levels ([§5 Structs](#structs)). | tests/field_visibility_levels.rs (§5 Structs) |
| s4.88 | Packages and manifests | `kara.toml` is the package manifest: the single source of package metadata, dependencies and build settings. | every package cell |
| s4.89 | Packages and manifests | **Layout.** | header |
| s4.90 | Packages and manifests | The compiler finds every `.kara` file under `src/`. | module_tree_and_import_forms |
| s4.91 | Packages and manifests | `examples/` is a separate root: each file is a program run with `karac run --example NAME`. | tests/run_example_package.rs |
| s4.92 | Packages and manifests | **Finding the package.** A package-mode command (`karac build`) walks up from the working directory to the first `kara.toml`; none is `E0227 | not_inside_a_project_refused |
| s4.93 | Packages and manifests | `karac run file.kara` needs no manifest. | every single-file cell (cases.py) |
| s4.94 | Packages and manifests | For a single file it walks up from the file's own directory, not the working directory, so a script inside a package gets that package's dep | script_walks_up_from_its_own_directory; script_no_manifest_refused |
| s4.95 | Packages and manifests | `--manifest path` forces a manifest; `--no-manifest` forces none. | script_no_manifest_refused (--no-manifest); tests/cli.rs (--manifest) |
| s4.96 | Packages and manifests | **The manifest.** | header |
| s4.97 | Packages and manifests | - `[package].name` is required. | manifest_name_required_refused |
| s4.98 | Packages and manifests | It is the package's identity, and it gives the name dependents use as a path's first segment, so with each `-` read as `_` it must be an ide | package_name_not_identifier_refused, package_name_keyword_refused |
| s4.99 | Packages and manifests | - **Hyphens in a package name.** A package name may contain `-`, and a path segment may not, so a dependent's path starts with the name with | path_dependencies_and_hyphen_names, package_name_dash_accepted |
| s4.100 | Packages and manifests | Two dependencies whose names differ only by `-` against `_` would claim one segment, which is an error at the manifest; an `alias` resolves  | dependency_names_differing_by_hyphen_refused; NEW dependency_names_differing_by_hyphen_alias_resolves |
| s4.101 | Packages and manifests | - `[package].import` is optional. | dependency_declared_import_name |
| s4.102 | Packages and manifests | It declares the import name dependents use instead of the name, and must be an identifier: `kara-data` declares `import = "data"`, so its de | dependency_declared_import_name, dependency_declared_import_name_replaces_name_refused, dependency_import_name_not_identifier_refused |
| s4.103 | Packages and manifests | A dependent's `alias` overrides both. | dependency_alias_overrides_import_name |
| s4.104 | Packages and manifests | - `[package].edition` is optional and validated. | manifest_unknown_edition_refused, manifest_edition_2026 |
| s4.105 | Packages and manifests | - `version` and `authors` are recognized. | manifest_version_not_semver_refused, authors_not_a_list_refused |
| s4.106 | Packages and manifests | `version` is a semver version, `MAJOR.MINOR.PATCH`, and anything else is an error (review-thread ruling 2026-11-16). | manifest_version_not_semver_refused |
| s4.107 | Packages and manifests | `authors` is a list of strings; any other value is an error (review-thread ruling 2026-11-21). | authors_not_a_list_refused |
| s4.108 | Packages and manifests | An unknown key in `[package]` and an unknown top-level table (`[dependecies]`) are warnings; invalid TOML is an error. | manifest_unknown_package_key_warns, unknown_top_level_table_warns, manifest_invalid_toml_refused |
| s4.109 | Packages and manifests | - `[build].target` selects the default manifest overlay ([Conditional compilation](#conditional-compilation)); `[build].targets` lists the t | design/targets: check_targets_override, build_target_flag_builds_one |
| s4.110 | Packages and manifests | - **Dependencies in 0.8.** `check`, `run`, `build` and `test` resolve dependencies the same way. | dependency_cycle_refused_in_every_command, registry_dependency_refused_in_every_command |
| s4.111 | Packages and manifests | A path dependency (`{ path = "../logging" }`) resolves, type-checks across the package boundary and links. | path_dependencies_and_hyphen_names (run + build) |
| s4.112 | Packages and manifests | A git dependency (`{ git = "<url>" }`, optionally with one of `branch`, `tag` or `rev`) is fetched; a fetch that fails is `E_GIT_FETCH_FAILE | tests/git_fetch_e2e.rs |
| s4.113 | Packages and manifests | A registry dependency (`"1.2"`, `{ version = .. }`) is fetched through the registry proxy named by `KARAC_REGISTRY_PROXY`. | tests/registry_fetch_e2e.rs (needs a proxy) |
| s4.114 | Packages and manifests | With no proxy configured it is the error `E_REGISTRY_DEP_UNSUPPORTED`, with a help line to use a `path` dependency on a local checkout inste | registry_dependency_refused_in_every_command |
| s4.115 | Packages and manifests | A dependency cycle is `E_DEPENDENCY_CYCLE` in every command. | dependency_cycle_refused_in_every_command |
| s4.116 | Packages and manifests | **Editions.** | header |
| s4.117 | Packages and manifests | - `"2026"` is the only edition. | manifest_unknown_edition_refused |
| s4.118 | Packages and manifests | An unknown value is an error: `unknown edition "2027"; this compiler supports editions up to "2026"`. | manifest_unknown_edition_refused |
| s4.119 | Packages and manifests | - Without the field, the edition is `"2026"`: the default is always the earliest edition, never a silent upgrade. | manifest_edition_2026 |
| s4.120 | Packages and manifests | - Editions are per package. | one edition exists, nothing to observe; no cell |
| s4.121 | Packages and manifests | A newer compiler compiles every earlier edition, and a library's edition does not affect its users. | one edition exists, nothing to observe; no cell |
| s4.122 | Packages and manifests | - An edition gates breaking language changes. | one edition exists, nothing to observe; no cell |
| s4.123 | Packages and manifests | One example exists: an edition boundary may broaden private-function effect inference ([§2](#2-specification-layers)). | one edition exists, nothing to observe; no cell |
| s4.124 | Packages and manifests | - **Migration pipeline.** An edition-gated change goes through three stages: 1. | no edition-gated change exists yet; no cell |
| s4.125 | Packages and manifests | **Warn.** A warn-by-default lint reports the coming change in the current edition, suppressible with `#[allow(<lint>)]`. | no edition-gated change exists yet; no cell |
| s4.126 | Packages and manifests | The lint name is permanent ([§17](#17-tooling-contract)). 2. | no edition-gated change exists yet; no cell |
| s4.127 | Packages and manifests | **Deny.** Later in the edition, the lint becomes deny-by-default; code needs an explicit `#[allow(<lint>)]` to compile. 3. | no edition-gated change exists yet; no cell |
| s4.128 | Packages and manifests | **Error.** At the next edition boundary the behavior becomes a hard error under the new edition. | no edition-gated change exists yet; no cell |
| s4.129 | Packages and manifests | The warning stage gives authors lead time, the deny stage forces awareness without forcing immediate migration, and the edition boundary is  | no edition-gated change exists yet; no cell |
| s4.130 | Packages and manifests | With the first edition boundary, `karac explain --edition <NEXT>` will show a package's pending migrations and `karac fix --edition <NEXT>`  | no edition-gated change exists yet; no cell |
| s4.131 | Packages and manifests | Lint attributes can escalate or allow a lint ([§17 Lint levels](#lint-levels)). | no edition-gated change exists yet; no cell |
| s4.132 | Packages and manifests | A `[lints]` table in `kara.toml` comes with the tooling track ([deferred.md](deferred.md#tooling)). | not implemented in 0.8 (the page says so, or the command does not exist yet) |
| s4.133 | Packages and manifests | **Versions and resolution.** | header |
| s4.134 | Packages and manifests | - **Semver.** `"1.2"` means `>=1.2.0, <2.0.0`; `"=1.2.3"` is exact; `">=1.0, <1.5"` is a range. | registry resolution, needs a proxy: tests/registry_fetch_e2e.rs covers fetch; conflict chains not covered |
| s4.135 | Packages and manifests | A conflict is a compile error showing the constraint chain. | registry resolution, needs a proxy: tests/registry_fetch_e2e.rs covers fetch; conflict chains not covered |
| s4.136 | Packages and manifests | - **PubGrub resolver.** The newest compatible version is chosen unless `kara.lock` pins one, and a conflict is explained as a chain ("`A` re | registry resolution, needs a proxy: tests/registry_fetch_e2e.rs covers fetch; conflict chains not covered |
| s4.137 | Packages and manifests | Rejected alternatives: npm-style duplicate copies (large builds, the same vulnerable package N times), Go-style minimum versions (ages depen | registry resolution, needs a proxy: tests/registry_fetch_e2e.rs covers fetch; conflict chains not covered |
| s4.138 | Packages and manifests | - **Workspaces.** A root `kara.toml` may declare members that share one lockfile and one output directory. | workspace_dependency |
| s4.139 | Packages and manifests | Shared versions go in `[workspace.dependencies]`, and a member opts in per dependency: | workspace_dependency |
| s4.140 | Packages and manifests | ```toml # workspace root kara.toml [workspace] members = ["core", "cli", "web"] | workspace_dependency |
| s4.141 | Packages and manifests | [workspace.dependencies] http = "1.2" json = "0.8" ``` | workspace_dependency |
| s4.142 | Packages and manifests | ```toml # core/kara.toml [dependencies] http = { workspace = true }    # version from [workspace.dependencies] json = { workspace = true } ` | workspace_dependency |
| s4.143 | Packages and manifests | `workspace = true` for a name missing from `[workspace.dependencies]` is a compile error. | workspace_dependency_not_declared_refused |
| s4.144 | Packages and manifests | A member may also declare its own dependencies with its own constraints. | workspace_dependency (cli's own path dependency) |
| s4.145 | Packages and manifests | **Lockfile.** `kara.lock` records every package in the resolved graph: name, exact version, source (proxy mirror or git URL), BLAKE3 content | not implemented in 0.8 (the page says so, or the command does not exist yet) |
| s4.146 | Packages and manifests | One lockfile serves every target, so cross-compiling never skews versions. | not implemented in 0.8 (the page says so, or the command does not exist yet) |
| s4.147 | Packages and manifests | Executables commit it; libraries do not. | not implemented in 0.8 (the page says so, or the command does not exist yet) |
| s4.148 | Packages and manifests | `karac update` moves every dependency to the newest compatible version; `karac update <pkg>` moves only `<pkg>` and what it newly needs. | not implemented in 0.8 (the page says so, or the command does not exist yet) |
| s4.149 | Packages and manifests | **Package identity and the proxy.** A package is identified by its git URL (`git+https://github.com/example/json-kara`). | not implemented in 0.8 (the page says so, or the command does not exist yet) (proxy, identity) |
| s4.150 | Packages and manifests | There is no central namespace and no publish step. | not implemented in 0.8 (the page says so, or the command does not exist yet) (proxy, identity) |
| s4.151 | Packages and manifests | The package proxy (`proxy.kara-lang.org` or a self-hosted equivalent) mirrors every URL and ref resolved through it, immutably, addressed by | not implemented in 0.8 (the page says so, or the command does not exist yet) (proxy, identity) |
| s4.152 | Packages and manifests | Resolution goes through the proxy by default; `--no-proxy` fetches directly. | not implemented in 0.8 (the page says so, or the command does not exist yet) (proxy, identity) |
| s4.153 | Packages and manifests | The proxy is what makes decentralized identity reproducible: a force-pushed branch, a deleted repository or a network failure cannot change  | not implemented in 0.8 (the page says so, or the command does not exist yet) (proxy, identity) |
| s4.154 | Packages and manifests | A central registry is not planned; the manifest accepts an optional `registry = "https://..."` so one can be added later without a break. | not implemented in 0.8 (the page says so, or the command does not exist yet) (proxy, identity) |
| s4.155 | Packages and manifests | **Reproducible builds.** Given a committed `kara.lock`, a pinned toolchain and the same target triple, builds are bit-identical. | NEW build_is_bit_identical (same package built twice, same bytes) |
| s4.156 | Packages and manifests | The compiler must be deterministic: no timestamps, sorted symbol output, no host paths in binaries, no nondeterministic linking. | NEW build_is_bit_identical (same package built twice, same bytes) |
| s4.157 | Packages and manifests | Any nondeterminism is a compiler bug. | NEW build_is_bit_identical (same package built twice, same bytes) |
| s4.158 | Packages and manifests | A package is the reproducible form: its files are named from the package root. | every package cell |
| s4.159 | Packages and manifests | A single-file build records the file's path as it was typed (a panic location reads `../p/oob.kara:3:13`), so an absolute path puts a host p | NEW single_file_panic_path_as_typed |
| s4.160 | Packages and manifests | **Toolchain pinning and library MSRV.** | header |
| s4.161 | Packages and manifests | - A library states the oldest compiler it supports with `[package].kara-version = ">=0.42"`; the resolver enforces it. | kara_version_requirement_refused |
| s4.162 | Packages and manifests | - A project pins the exact compiler in an optional `karac-toolchain.toml` at the project or workspace root (`version = "0.42"`, optional `ta | toolchain_pin_refused_in_every_command |
| s4.163 | Packages and manifests | A toolchain manager installs that version before running `karac`. | toolchain manager is outside karac; no cell |
| s4.164 | Packages and manifests | - `karac check`, `build`, `run` and `test` all refuse a package whose pin the running compiler does not satisfy, with `E_TOOLCHAIN_VERSION_M | toolchain_pin_refused_in_every_command, toolchain_pin_unparsable_refused |
| s4.165 | Packages and manifests | `check` holds to it like the others because a different compiler gives different diagnostics, and `check` is where the Mend loop starts. | rationale or cross-reference, no cell |
| s4.166 | Packages and manifests | The language server only warns, at the top of the file, and keeps checking: an editor that goes dark over a pin helps nobody (review-thread  | kara-lsp tests (tests/editor_check.rs) |
| s4.167 | Packages and manifests | - `karac fix` makes one manifest edit: a `version` written as a bare number in `karac-toolchain.toml` (`version = 0.8`, or `version = 0.8.1` | tests/toolchain_pin_fix.rs |
| s4.168 | Packages and manifests | A manifest is project source, and that edit is determined. | rationale or cross-reference, no cell |
| s4.169 | Packages and manifests | Moving an item to another file is a refactor, so `E_TEST_OUTSIDE_TEST_FILE` keeps its help and carries no edit (review-thread ruling 2026-11 | tests/test_block_outside_test_file.rs |
| s4.170 | Packages and manifests | - They are separate because the MSRV travels with the package and is read by the resolver, while the pin belongs to one checkout and is read | rationale or cross-reference, no cell |
| s4.171 | Packages and manifests | This mirrors Rust's `rust-version` and `rust-toolchain.toml`. | rationale or cross-reference, no cell |
| s4.172 | Packages and manifests | **Per-target dependencies and profiles.** | header |
| s4.173 | Packages and manifests | Each `karac build --target X` produces one artifact. | design/targets: build_target_flag_builds_one |
| s4.174 | Packages and manifests | Building a matrix of targets is a CI concern. | rationale or cross-reference, no cell |
| s4.175 | Packages and manifests | **Vendoring.** `karac vendor` copies the resolved dependencies into `vendor/`; `karac build --offline` then reads only `vendor/` and makes n | not implemented in 0.8 (the page says so, or the command does not exist yet) (vendor, cache, clean) |
| s4.176 | Packages and manifests | **Build cache.** Each package's own output goes in `dist/` (in the default `.gitignore`; `karac clean` removes it). | every build cell (dist/native) |
| s4.177 | Packages and manifests | Compiled dependencies go in the machine-wide `~/.kara/cache/`, keyed by compiler version, package version, edition, profile and target tripl | not implemented in 0.8 (the page says so, or the command does not exist yet) (vendor, cache, clean) |
| s4.178 | Packages and manifests | A compiler upgrade invalidates the cache; time never does. | not implemented in 0.8 (the page says so, or the command does not exist yet) (vendor, cache, clean) |
| s4.179 | Packages and manifests | `karac clean --global` evicts it. | not implemented in 0.8 (the page says so, or the command does not exist yet) (vendor, cache, clean) |
| s4.180 | Packages and manifests | **`karac init`.** `karac init [<name>] [--bin / --lib / --backend / --cli] [--force]` scaffolds a package. | init_bin, init_lib |
| s4.181 | Packages and manifests | - Bare `karac init` uses the working directory; `karac init <name>` creates `./<name>/`, and refuses if it exists and is not empty. | init_bin, init_nonempty_directory_refused |
| s4.182 | Packages and manifests | - The directory name is the package name and the root module name. | init_bin |
| s4.183 | Packages and manifests | It must match `[a-z][a-z0-9_]*` and not be a keyword; otherwise nothing is written (no automatic rewriting of `my-project`). | init_invalid_name_refused |
| s4.184 | Packages and manifests | - `--bin` (the default), `--lib`, `--backend` and `--cli` are exclusive. | init_kinds_exclusive_refused |
| s4.185 | Packages and manifests | A package is a binary or a library; a library plus a CLI is a workspace. | rationale or cross-reference, no cell |
| s4.186 | Packages and manifests | `--backend` and `--cli` write `karac new`'s scaffolds of the same name (below) in place of the hello-world `main`. | new_backend_and_cli_check |
| s4.187 | Packages and manifests | - It refuses if `kara.toml`, `src/main.kara` or `src/lib.kara` exists, unless `--force`. | init_existing_manifest_refused |
| s4.188 | Packages and manifests | It never overwrites `README.md`, the test file or `.gitignore`, even with `--force`. | NEW init_force_keeps_readme_no_git |
| s4.189 | Packages and manifests | It does not run `git init`. | NEW init_force_keeps_readme_no_git (.files: no .git) |
| s4.190 | Packages and manifests | - It writes five files: `kara.toml` (`name`, `version = "0.1.0"`, `authors = []`, `edition = "2026"`, an empty `[dependencies]`), `src/main. | init_bin (.files), init_lib |
| s4.191 | Packages and manifests | **`karac new`.** `karac new <name> [--backend / --cli / --lib]` creates `./<name>/` and scaffolds into it, with `karac init`'s name rules an | new_backend_and_cli_check |
| s4.192 | Packages and manifests | Its default is `--backend`: `src/main.kara` serves `127.0.0.1:8080` with `Server.serve_ws`, answering `/health`, echoing WebSocket messages  | new_backend_and_cli_check |
| s4.193 | Packages and manifests | `--cli` writes a small `wc`: `src/main.kara` reads the file named by `env.args()[1]` with `fs.read_to_string` and prints its lines, words an | new_backend_and_cli_check |
| s4.194 | Packages and manifests | `--lib` is `karac init --lib`'s. | new_backend_and_cli_check |
| s4.195 | Packages and manifests | `--data` is reserved and reports that its scaffold is deferred. | new_data_reserved_refused |
| s4.196 | Packages and manifests | **Command phasing.** | phasing table, no cell |
| s4.197 | Packages and manifests | / Phase / Commands / Gate / | phasing table, no cell |
| s4.198 | Packages and manifests | / First / `build`, `run`, `check`, `fmt`, `query`, `test`, `init`, `new`, `doc` / Current scope / | phasing table, no cell |
| s4.199 | Packages and manifests | / With dependency resolution / `update`, `clean`, `install`, `vendor` / The resolver and lockfile / | phasing table, no cell |
| s4.200 | Packages and manifests | / Later / `bench`, `publish`, `audit` / A bench harness, a publish protocol, a vulnerability database / | phasing table, no cell |
| s4.201 | Packages and manifests | `karac fix` applies the machine-applicable fixes of `--output=jsonl` and is not tied to a phase. | phasing table, no cell |
| s4.202 | Constants | `const NAME: T = e;` declares a constant. | constants |
| s4.203 | Constants | It is the only value declared at module scope: Kāra has no module-level `let` or `let mut`. | module_let_mut_refused; NEW module_let_refused |
| s4.204 | Constants | **The initializer is a compile-time constant.** There is no module initialization: no code runs before `main`, so there is no initialization | rationale or cross-reference, no cell |
| s4.205 | Constants | Constants exist in the binary as data, and at program start the runtime calls `main`. | rationale or cross-reference, no cell |
| s4.206 | Constants | **Allowed initializers:** | header |
| s4.207 | Constants | - Literals: `42`, `3.14`, `true`, `'x'`, and string literals at type `Str` (`"localhost"`), which view static data and allocate nothing ([St | constants (literals, Str) |
| s4.208 | Constants | - Arithmetic, comparison and boolean operations on constants: `60 * 1000`, `MAX_RETRIES + 1`. | constants (TIMEOUT_MS, NEXT, BIG) |
| s4.209 | Constants | - Enum variants with constant arguments: `Direction.North`, `Some(42)`. | constants (OPT) |
| s4.210 | Constants | - Struct and tuple literals of constants: `Point { x: 0, y: 0 }`, `(1, 2, 3)`. | constants (ORIGIN, T) |
| s4.211 | Constants | - List literals and repeat forms at an `Array` type: `[1, 2, 3]`, `[0; 512]`. | constants (A, Z) |
| s4.212 | Constants | - Other constants, from the same module or imported ones. | constants (same module); NEW constant_imported_in_initializer |
| s4.213 | Constants | **Forbidden initializers:** | header |
| s4.214 | Constants | - Any expression with an effect, declared or inferred. | constant_initializer_effect_named, constant_block_initializer_effect_named |
| s4.215 | Constants | - Function calls. | constant_function_call_refused |
| s4.216 | Constants | Calls to `const fn` come with the comptime track. | not implemented in 0.8 (the page says so, or the command does not exist yet) |
| s4.217 | Constants | - Closures, which may capture state and carry effects. | NEW constant_closure_refused |
| s4.218 | Constants | - Anything that allocates. | constant_string_type_refused |
| s4.219 | Constants | `String` is heap-allocated, so it cannot be a constant's type; use `Str`. | constant_string_type_refused |
| s4.220 | Constants | A refused initializer is reported at the expression, and when what it calls has an effect, a note names the effect at the call: `const X: i6 | constant_initializer_effect_named |
| s4.221 | Constants | A constant declared in a block is visible to the rest of the block only, so it takes no visibility keyword; `pub const` there is an error. | block_constant_visibility_refused |
| s4.222 | Constants | The diagnostic for an effectful initializer points at the effectful expression, names its effect, and suggests the `main`-and-context patter | constant_initializer_effect_named |
| s4.223 | Constants | Compile-time string concatenation (`"http://" + "localhost"`) also waits for `const fn`; build such strings at run time. | not implemented in 0.8 (the page says so, or the command does not exist yet) |
| s4.224 | Constants | **Each use is a fresh value.** A use of a constant is a new value built from its initializer. | constants (p and q from ORIGIN) |
| s4.225 | Constants | For a `Copy` type this is a copy. | constants (p and q from ORIGIN) |
| s4.226 | Constants | For any other type (`const ORIGIN: Point` above, when `Point` is not `Copy`), every use builds its own value, which the user then owns and m | constants (p and q from ORIGIN) |
| s4.227 | Constants | **A block may declare a constant.** `const MOD: i64 = 1_000_000_007;` is also a statement in any block, a function body included. | constants (MOD in main) |
| s4.228 | Constants | It follows the rules above for its type and initializer, and its scope is the rest of the enclosing block: it is not visible before its decl | block_constant_scope_refused, block_constant_before_declaration_refused, block_constant_visibility_refused |
| s4.229 | Constants | It may name earlier constants, at module scope or in an enclosing block, but no local binding (`let`, a parameter, a pattern), since those a | constant_names_local_refused; NEW block_constant_shadows_like_let (names module and enclosing-block constants) |
| s4.230 | Constants | It shadows and is shadowed like a `let`. | NEW block_constant_shadows_like_let |
| s4.231 | Constants | It has no address identity across calls; each use is a fresh value as above (review-thread ruling 2026-11-08, B-2026-11-08-60). | constants (fresh values) |
| s4.232 | Constants | **Visibility.** Constants take the three levels of [Visibility](#visibility): `pub const PI: f64 = 3.14159;`, `const INTERNAL: i64 = 42;`, ` | NEW constant_default_visibility_package_wide, constant_private_other_directory_refused |
| s4.233 | Constants | A `pub` constant is a semver commitment on its type and its value: changing either, or removing it, breaks users, as changing a `pub fn` sig | rationale or cross-reference, no cell |
| s4.234 | Constants | **Why no run-time initialization.** Eager initialization (module code at startup), lazy initialization (on first access) and a separate `sta | rationale or cross-reference, no cell |
| s4.235 | Constants | Kāra's effect discipline requires every effect to have a source. | rationale or cross-reference, no cell |
| s4.236 | Constants | A module-scope `load_config("app.toml")` would produce `reads(FileSystem)` with no function to charge it to, no path for its error, and no a | rationale or cross-reference, no cell |
| s4.237 | Constants | Constant initializers remove those questions by removing the run-time step. | rationale or cross-reference, no cell |
| s4.238 | Constants | **No mutable globals.** There is no module-level `let mut` and no `static mut`. | module_let_mut_refused |
| s4.239 | Constants | Program-wide mutable state is a value created in `main` and passed down (below). | pattern description, no cell |
| s4.240 | Constants | Module-level cells (`LazyLock`, `OnceLock`, `OnceCell`) and `#[thread_local]` are deferred to the systems track ([deferred.md](deferred.md#s | not implemented in 0.8 (the page says so, or the command does not exist yet) |
| s4.241 | Constants | **Run-time values: the `main`-and-context pattern.** Values that need run-time initialization (configuration, database pools, compiled regul | pattern description, no cell |
| s4.242 | Constants | 1. | pattern description, no cell |
| s4.243 | Constants | **Every effect has a source.** `reads(FileSystem)` starts in `load_config`, passes through `AppContext.new`, and ends at `main`'s signature. | pattern description, no cell |
| s4.244 | Constants | `main`'s effects list everything the program can do. 2. | pattern description, no cell |
| s4.245 | Constants | **Failure is a value.** A failed initialization returns `Err` through `?`; `main` returns it and the runtime prints it and exits 1. | pattern description, no cell |
| s4.246 | Constants | Nothing can fail before `main` starts. 3. | pattern description, no cell |
| s4.247 | Constants | **No pervasive parameter.** Functions that use the context are methods that borrow `self`. | pattern description, no cell |
| s4.248 | Constants | Only the few functions at the top of the call graph (`main`, `run_server`) pass `ctx` explicitly. | pattern description, no cell |
| s4.249 | Constants | Pure leaf functions (`format_date`, `validate_email`) take neither. 4. | pattern description, no cell |
| s4.250 | Constants | **Testing is direct.** A test builds an `AppContext` and calls its methods; there is no global state to patch. | pattern description, no cell |
| s4.251 | Constants | **Request-scoped state** (the logged-in user, a request id, a trace span) is a second, per-request context passed beside `self`: | pattern description, no cell |
| s4.252 | Constants | `AppContext` lives for the program and `RequestContext` for one request. | pattern description, no cell |
| s4.253 | Constants | Neither is a global. | pattern description, no cell |
| s4.254 | Conditional compilation | Most code needs none. | pattern description, no cell |
| s4.255 | Conditional compilation | A function compiles for a target when every resource its effects reach is provided by that target, so code that uses only portable effects b | design/targets effect-gating cells |
| s4.256 | Conditional compilation | Two mechanisms cover the rest: the `#[cfg]` attribute, and a platform suffix on a file name. | rationale or cross-reference, no cell |
| s4.257 | `#[cfg(...)]` | `#[cfg]` compiles the declaration it marks only when its condition holds. | cfg_not_implemented_refused (#[cfg] not implemented, §4 says so) |
| s4.258 | `#[cfg(...)]` | **Where it may appear.** On items, on impl members, on struct fields and on enum variants. | cfg_not_implemented_refused (#[cfg] not implemented, §4 says so) |
| s4.259 | `#[cfg(...)]` | Not on statements. | cfg_not_implemented_refused (#[cfg] not implemented, §4 says so) |
| s4.260 | `#[cfg(...)]` | **Keys.** | cfg_not_implemented_refused (#[cfg] not implemented, §4 says so) |
| s4.261 | `#[cfg(...)]` | - `target`: the compilation target, one of the target names of [§16](#the-v1-target-set) (`"native"`, `"wasm_wasi"`). | cfg_not_implemented_refused (#[cfg] not implemented, §4 says so) |
| s4.262 | `#[cfg(...)]` | - `target_os`: the OS platform: `"linux"`, `"macos"`, `"windows"` or `"wasm"`, the same values as the file suffixes below. | cfg_not_implemented_refused (#[cfg] not implemented, §4 says so) |
| s4.263 | `#[cfg(...)]` | An unknown key or value is an error. | cfg_not_implemented_refused (#[cfg] not implemented, §4 says so) |
| s4.264 | `#[cfg(...)]` | **Grammar.** The condition is one predicate. | cfg_not_implemented_refused (#[cfg] not implemented, §4 says so) |
| s4.265 | `#[cfg(...)]` | A predicate is `key: "value"`, or a combinator around predicates: | cfg_not_implemented_refused (#[cfg] not implemented, §4 says so) |
| s4.266 | `#[cfg(...)]` | - `all(p, ...)` holds when every `p` holds; | cfg_not_implemented_refused (#[cfg] not implemented, §4 says so) |
| s4.267 | `#[cfg(...)]` | - `any(p, ...)` holds when at least one `p` holds; | cfg_not_implemented_refused (#[cfg] not implemented, §4 says so) |
| s4.268 | `#[cfg(...)]` | - `not(p)` holds when `p` does not. | cfg_not_implemented_refused (#[cfg] not implemented, §4 says so) |
| s4.269 | `#[cfg(...)]` | **Rules.** | cfg_not_implemented_refused (#[cfg] not implemented, §4 says so) |
| s4.270 | `#[cfg(...)]` | - A declaration without `#[cfg]` exists on every target; effect-driven gating alone decides whether it compiles. | cfg_not_implemented_refused (#[cfg] not implemented, §4 says so) |
| s4.271 | `#[cfg(...)]` | - A declaration whose condition does not hold does not exist on that target. | cfg_not_implemented_refused (#[cfg] not implemented, §4 says so) |
| s4.272 | `#[cfg(...)]` | - Two `#[cfg]` attributes on one declaration are an error; combine them with `all(...)` or `any(...)`. | cfg_not_implemented_refused (#[cfg] not implemented, §4 says so) |
| s4.273 | `#[cfg(...)]` | - Naming an item that does not exist on the current target is an error at the use, naming the item's condition: `cannot reference item 'foo' | cfg_not_implemented_refused (#[cfg] not implemented, §4 says so) |
| s4.274 | `#[cfg(...)]` | - There is no run-time `if target == ...`. | cfg_not_implemented_refused (#[cfg] not implemented, §4 says so) |
| s4.275 | `#[cfg(...)]` | `#[cfg]` is the one mechanism. | cfg_not_implemented_refused (#[cfg] not implemented, §4 says so) |
| s4.276 | `#[cfg(...)]` | - Under `karac check` with several targets, each target's pass sees exactly the declarations that target's build sees ([§16](#checking-sever | cfg_not_implemented_refused (#[cfg] not implemented, §4 says so) |
| s4.277 | `#[cfg(...)]` | `#[cfg]` is not yet implemented. | cfg_not_implemented_refused |
| s4.278 | `#[cfg(...)]` | Until it is, `#[target(native)]` or `#[target(wasm_wasi)]` gates an item on one target, with the same rules (the item does not exist elsewhe | target_gated_item, target_gated_item_absent_elsewhere_refused, target_attribute_twice_refused, target_attribute_on_statement_refused |
| s4.279 | `#[cfg(...)]` | `#[target]` is an interim spelling: when `#[cfg]` lands it becomes a deprecation warning whose fix rewrites it to `#[cfg(target: "...")]`. | not implemented in 0.8 (the page says so, or the command does not exist yet) |
| s4.280 | `#[cfg(...)]` | `#[target(gpu)]` and `#[target(wasm_browser)]` name later tracks' targets ([§16](#16-targets-and-compilation)). | design/targets: later_track_target_attribute_warns |
| s4.281 | `#[cfg(...)]` | They are accepted, so code written for those tracks still parses, with a `later_track` warning: an item gated only on them does not exist in | design/targets: later_track_target_attribute_warns |
| s4.282 | Platform files | A file whose name ends in an OS suffix (`_linux`, `_macos`, `_windows`, `_wasm`) compiles only for that OS. | platform_files_and_plain_underscore_stem |
| s4.283 | Platform files | It is the file-level form of `#[cfg(target_os: ...)]`. | platform_files_and_plain_underscore_stem |
| s4.284 | Platform files | **Recognizing a platform file.** The walker splits the file stem at its last underscore. | platform_files_and_plain_underscore_stem (foo_linux_x86_64) |
| s4.285 | Platform files | If the tail is exactly one of the four suffixes, the file is a platform file. | platform_files_and_plain_underscore_stem (foo_linux_x86_64) |
| s4.286 | Platform files | Otherwise it is an ordinary module whose name is the whole stem: `foo_linux_x86_64.kara` is the module `foo_linux_x86_64` and always compile | platform_files_and_plain_underscore_stem (foo_linux_x86_64) |
| s4.287 | Platform files | This leaves the rest of the underscore namespace free for later extensions. | platform_files_and_plain_underscore_stem (foo_linux_x86_64) |
| s4.288 | Platform files | **A platform file replaces a shared file.** When `poller_linux.kara` and `poller.kara` both exist, they are the same module `poller`, and ex | platform_files_and_plain_underscore_stem, platform_file_absent_uses_shared |
| s4.289 | Platform files | They do not merge. | platform_files_and_plain_underscore_stem, platform_file_absent_uses_shared |
| s4.290 | Platform files | A `pub fn` that exists only in `poller.kara` does not exist on Linux, and importing it there is `E0113 UnknownItemInModule`, listing the pla | platform_file_replaces_shared_refused |
| s4.291 | Platform files | Common helpers therefore go in a third module that both import. | rationale or cross-reference, no cell |
| s4.292 | Platform files | Two files that both survive for one target under one module path (two shared files, or two with the same suffix) are a duplicate module, rej | cannot be constructed: one path holds one file, and no other spelling maps two survivors to one module; no cell |
| s4.293 | Platform files | A symbol-level conflict between the files of one module cannot arise, so it has no diagnostic: `E0226 ConflictingPlatformModule`, reserved f | retired code, no cell |
| s4.294 | Platform files | **Missing platforms.** If platform files cover some operating systems and no shared file exists, the module does not exist on the others, an | platform_module_missing_refused |
| s4.295 | Platform files | `karac check --platform=all` checks the package once per platform and fails if any fails, so one machine can verify every OS. | platform_module_missing_refused (--platform=all) |
| s4.296 | Platform files | There is no compiler check that every OS is covered; that is a CI concern. | rationale or cross-reference, no cell |
| s4.297 | Platform files | **Three meanings of "target".** | design/targets cells; platform_file_absent_uses_shared (--platform=wasm) |
| s4.298 | Platform files | - The **compilation target** of [§16](#16-targets-and-compilation), which `#[cfg(target: ...)]` tests. | design/targets cells; platform_file_absent_uses_shared (--platform=wasm) |
| s4.299 | Platform files | `--target=<name>` chooses it; the default is `native`. | design/targets cells; platform_file_absent_uses_shared (--platform=wasm) |
| s4.300 | Platform files | - The **manifest overlay triple**, which selects the `[target.<triple>]` blocks to merge. | design/targets cells; platform_file_absent_uses_shared (--platform=wasm) |
| s4.301 | Platform files | `--target=<triple>` (any value that is not a target name) chooses it, else `[build].target`, else the host triple. | design/targets cells; platform_file_absent_uses_shared (--platform=wasm) |
| s4.302 | Platform files | - The **OS platform**, which `#[cfg(target_os: ...)]` and the file suffixes test. | design/targets cells; platform_file_absent_uses_shared (--platform=wasm) |
| s4.303 | Platform files | `karac check --platform=<name>` chooses it. | design/targets cells; platform_file_absent_uses_shared (--platform=wasm) |
| s4.304 | Platform files | Otherwise a wasm compilation target selects `wasm`, and every other target selects the host OS. | design/targets cells; platform_file_absent_uses_shared (--platform=wasm) |
| s4.305 | Platform files | `[build].target` never affects it. | design/targets cells; platform_file_absent_uses_shared (--platform=wasm) |
| s4.306 | Platform files | `--platform` is accepted only by `karac check`. | platform_flag_only_on_check_refused |
| s4.307 | Platform files | Analysing another OS's half from any machine is what keeps a platform split maintainable; emitting it is not possible, because code generati | rationale or cross-reference, no cell |
| s4.308 | Platform files | `karac build` and `karac run` refuse `--platform` and name `check`. | platform_flag_only_on_check_refused |
| s4.309 | Platform files | Per-OS artifacts come from a CI matrix. | rationale or cross-reference, no cell |
| s4.310 | Platform files | Test binaries are built for the host OS. | rationale or cross-reference, no cell |
| s4.311 | No feature flags | Kāra has no Cargo-style `[features]`, in `kara.toml` or as `#[cfg(feature: ...)]`. | cfg_not_implemented_refused; NEW features_table_is_unknown_warns |
| s4.312 | No feature flags | Feature unification across a dependency graph is Cargo's largest practical pain ("compiles in `cargo test` but not `cargo build`"), and Kāra | rationale or cross-reference, no cell |
| s4.313 | No feature flags | The usual uses of feature flags are covered one by one: | rationale or cross-reference, no cell |
| s4.314 | No feature flags | - **Test-only dependencies:** `[dev-dependencies]`. | rationale or cross-reference, no cell |
| s4.315 | No feature flags | - **Target-dependent source:** `#[cfg]` and platform files. | rationale or cross-reference, no cell |
| s4.316 | No feature flags | - **Build configuration:** profiles ([§16](#16-targets-and-compilation)) are the one configuration axis. | rationale or cross-reference, no cell |
| s4.317 | No feature flags | - **Per-target dependencies:** `[target.<triple>.dependencies]`. | rationale or cross-reference, no cell |
| s4.318 | No feature flags | - **Optional library surface:** publish separate packages (`foo`, `foo-blocking`). | rationale or cross-reference, no cell |
| s4.319 | No feature flags | This is an established pattern and avoids the unification bugs, at the cost of more maintenance for the package author. | rationale or cross-reference, no cell |
| s4.320 | No feature flags | A constrained feature axis (per-package flags without transitive unification) is held for a later RFC, to be opened if separate packages pro | rationale or cross-reference, no cell |
| s4.321 | No feature flags | --- | rule |
