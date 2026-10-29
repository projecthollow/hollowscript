# 10. Modules and imports

A program is a set of source files called *modules*. One module is the *entry module*; the others are reached by `import`. Version 1 has relative-path imports resolved through a host-supplied loader, and no package manager. Nothing in this chapter prevents a package system being added later.

## 10.1 Program structure of a file

The grammar of a file is `file` in [section 18.3](18-grammar.md#183-files-and-top-level-items).

All `uses` lines ([section 9.2](09-effects-and-capabilities.md#92-declaring-and-granting)) come first, then all `import` lines, then everything else. A `uses` line after an import or a statement is HS0106; an `import` after a statement is HS0107. Comments may appear anywhere. An empty file is valid and produces no result, as is a file containing only `uses` lines, imports and comments.

The top level of a file is executed as described in [section 4.10](04-statements.md#410-top-level-declaration-order). A file has no `main`: its top-level statements are its program.

## 10.2 Import declarations

`import double, origin from "./helpers"` binds the names `double` and `origin` in the importing file, each to the exported declaration of that name in the module the path denotes.

- Names bind directly and keep their names. There is no renaming, no wildcard import, and no re-export: an importer cannot export what it imported.
- An imported name is a constant binding. It cannot be assigned, and it is subject to the no-shadowing rule ([section 4.6](04-statements.md#46-scope-and-shadowing)): importing a name that is already declared in the file, or importing the same name twice, is HS0201. An imported name that is a prelude name hides the prelude name, as a declaration does ([section 4.6](04-statements.md#46-scope-and-shadowing)).
- What can be imported is what the module exports ([section 10.4](#104-exports)). Importing a name that the module does not export, or that it declares without `export`, is HS0604. The error is reported whether or not the name is used.
- Importing an `enum` imports the enum type and its variants (which are reached as `EnumName.Variant`); importing a `record` imports the type name. Type names and value names share the import list, and the checker knows which kind each is. An import of a name imports every exported declaration of that name in both namespaces, so a module that exports a `record Foo` and a `constant Foo` (the namespaces do not conflict) gives both to the importer.
- An import that is never used is a warning (HS0802).
- For an import that closes a cycle ([section 10.5](#105-loading-and-initialisation-order)) only HS0603 is reported, and the names of that import are given the error type, so HS0604 is not reported in addition.

## 10.3 Import paths and canonical paths

The path is a string literal that contains no interpolation (HS0602). It is resolved to a *canonical path*, which identifies the module. Two imports whose paths resolve to the same canonical path denote one module, which is loaded and initialised once.

**Form.** An import path:

- begins with `./` or `../` (HS0602 otherwise, so there are no absolute paths, no bare names and no package names);
- has segments separated by a single `/`; an empty segment (`//`), a trailing `/`, a segment that consists only of dots other than exactly `.` or `..`, and any backslash are HS0602;
- has segments made only of ASCII letters, digits, `_`, `-` and `.` (HS0602 otherwise);
- omits the `.hws` extension, which is supplied by the resolver (HS0609 if the path ends in `.hws`);
- ends in a module name: its last segment is not `.` or `..` (HS0602);
- has no segment other than `.` and `..` that ends in `.`, and none whose part before its first `.` is, in any letter case, `con`, `prn`, `aux`, `nul`, `com1` to `com9` or `lpt1` to `lpt9` (HS0602), because such names are not portable between file systems.

**Resolution.** Every module has a canonical path: a `/`-separated sequence of segments ending in a file name with the extension `.hws`, relative to the *root*. The root is chosen by the host; by default it is the directory that contains the entry module. The canonical path of the entry module is its path relative to the root (`main.hws` if it is in the root). Canonical paths never begin with `./` or `../`, have no `.` or `..` segments, and are case-sensitive: `Util.hws` and `util.hws` are different modules whatever the host's file system does.

To resolve an import path `S` written in a module whose canonical path has directory segments `D`:

1. Start with the list `D`.
2. For each segment of `S` except the last: `.` changes nothing; `..` removes the last element of the list, and if the list is empty the import climbs above the root, which is HS0605; any other segment is appended to the list.
3. The last segment of `S` with `.hws` appended is the file name. The canonical path is the list followed by the file name, joined with `/`.

The name that a host gives for the entry module is converted to a canonical path by the same rules: a `\` is not a separator, a leading `./` and any `.` segments are removed, `..` segments are resolved, and a name that climbs above the root is rejected by the host. `./main.hws` and `main.hws` therefore denote one module, and an import of `"./main"` from another module denotes the entry module.

For example, from `app/main.hws`, `"./util"` is `app/util.hws`, `"../shared/util"` is `shared/util.hws`, and `"./sub/../util"` is `app/util.hws`, the same module as `"./util"`. From `main.hws` at the root, `"../secret"` is HS0605.

**Loading.** The host supplies a *loader*: a function from a canonical path to the bytes of the source, or to "not found". A path the loader reports as not found is HS0601, and one it cannot read for another reason is HS0607. The loader must not return content from outside the root, so a symbolic link or other indirection that leaves the root is treated as not found, and it must find the file without the check and the read being separate steps: it opens every file relative to a handle for the root, without following links, or serves the modules from an in-memory tree. It returns content only if the actual name of the file matches the requested canonical path segment by segment with the exact case, and otherwise reports not found (HS0601), so on a case-insensitive file system a request for `util.hws` never returns `Util.hws`. A program has at most 1,024 modules and 67,108,864 bytes of source in total; the import that would exceed either limit is HS0612 and the module is not loaded. The bytes are processed as in [section 1.1](01-lexical-structure.md#11-source-text).

## 10.4 Exports

`export` may prefix `fn`, `record`, `enum` and `constant` declarations at the top level. It may not prefix `variable` (HS0108): a mutable name shared between modules is hidden coupling, and a `fn` covers every use of one. An exported `constant` names a single identifier: `export` on a record pattern or a list pattern is HS0108, and the declaration is otherwise processed as if `export` were absent. An exported declaration is visible to importers by its name. An unexported declaration is private to its module. An exported `constant` is a value computed when the module is initialised.

An exported declaration is not reported as unused. An exported function may mention private types in its signature; importers can pass and return such values but cannot name the type. A record type is the same type everywhere it is spelled ([section 2.5](02-types.md#25-record-types)); an enum type is identified by its declaring module and name.

Exports of the *entry module* are also the functions a host may call ([Chapter 12](12-instances.md)).

**No capability leaves a module through an export.** The type of an exported `constant`, and the return type of an exported function, may not contain an effectful function type, directly or inside a record, list, map, set, optional or enum (HS0613). Otherwise `export constant p = print` would hand the capability to a module that has no `uses` line for it. A parameter of effectful type is allowed, because the caller supplies a capability that it has itself declared.

```hollow-module-error
// file: main.hws
import p from "./lib"
effect p("hello")
```

```hollow-module-error
// file: lib.hws
uses print

export constant p = print // HS0613
```

```hollow-error
record P = { x: Int, y: Int }
constant p: P = { x: 1, y: 2 }
export constant { x, y } = p // HS0108
return x + y
```

## 10.5 Loading and initialisation order

Before any statement of any module runs, the entry module and everything it imports, transitively, are loaded and checked ([section 2.11](02-types.md#211-when-types-are-checked)). All diagnostics are reported together, each carrying the canonical path of the module it belongs to and positions inside that module. If any is an error, nothing runs.

Then modules are initialised in *dependency order*: to initialise a module, first initialise each module it imports, in the order the `import` lines appear (skipping any that is already initialised), and then run the module's own top-level statements. A module's top-level statements therefore run once, the first time it is needed, and always after those of its imports. A top-level `return` is allowed only in the entry module (HS0611 elsewhere), and so is a top-level `?` ([section 8.3](08-errors-and-results.md#83-the--operator)). An imported module therefore always runs to its end, and every name it declares is initialised before any importer uses it.

An import cycle is HS0603, reported at the import in the module that closes the cycle, with the chain of canonical paths (`a.hws`, `b.hws`, `a.hws`) in the message. A module importing itself is a cycle.

Each module sees only the capabilities that it declares with its own `uses`.

```hollow-module
// file: main.hws
uses print
import a from "./a"
import b from "./b"

effect print("main ${a + b}")
```

```hollow-module
// file: a.hws
uses print

effect print("init a")
export constant a = 1
```

```hollow-module
// file: b.hws
uses print

effect print("init b")
export constant b = 2
```

Output of running `main.hws`:

```text
init a
init b
main 3
```

```hollow-module
// file: main.hws
import Point, origin from "./shapes"

constant p: Point = origin()
return p.x
```

```hollow-module
// file: shapes.hws
export record Point = { x: Int, y: Int }

export fn origin(): Point {
    return { x: 0, y: 0 }
}
```

The program returns `0`.

```hollow-module-error
// file: main.hws
import secret from "./m" // HS0604
return secret
```

```hollow-module-error
// file: m.hws
constant secret = 1
```

```hollow-module-error
// file: main.hws
import a from "./main" // HS0603
return a
```

## 10.6 Diagnostics across modules

Every diagnostic has a `file` (the canonical path of the module it is about), a span and a line ([Chapter 17](17-diagnostics.md)). An error inside an imported module is reported against that module with that module's own positions, never against the importer. When a module has errors, the modules that import it are still checked, and a name that failed to be declared properly is given the error type ([section 2.11](02-types.md#211-when-types-are-checked)), so the importers report only their own mistakes.

## 10.7 Versions and packages

Version 1 has no package manager, no version constraints and no way to import from anywhere except a relative path. A future version may add a form of `import` path that names a package; such paths do not begin with `.` and are currently HS0602, so adding them cannot change the meaning of an existing program.
