"""The hand-written stub `python/lobcore/lobcore.pyi` names exactly the public surface of the
extension module (top-level names, and every method / property of Book and RefBook), and every
function's parameter names and defaults equal the extension's ``__text_signature__`` (the
`#[pyo3(signature = ...)]` attributes): a stub default that drifts from the code, as the
`array_window` 1024-vs-2048 pitfall in docs/DESIGN.md section 9 did, fails here."""

from __future__ import annotations

import ast
import inspect
from pathlib import Path

import lobcore

STUB = Path(__file__).resolve().parents[1] / "lobcore" / "lobcore.pyi"


def stub_tree() -> ast.Module:
    return ast.parse(STUB.read_text())


def stub_names(tree: ast.Module) -> set[str]:
    names: set[str] = set()
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.ClassDef)):
            names.add(node.name)
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            names.add(node.target.id)
    return names


def stub_class_members(tree: ast.Module, cls: str) -> set[str]:
    node = next(n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == cls)
    return {m.name for m in node.body if isinstance(m, ast.FunctionDef) and m.name != "__init__"}


def module_public_names() -> set[str]:
    # maturin's pure-Rust layout re-exports the native submodule `lobcore.lobcore`; not part of the surface
    return {n for n in dir(lobcore) if (not n.startswith("_") or n == "__version__") and n != "lobcore"}


def class_public_members(cls) -> set[str]:
    return {n for n in vars(cls) if not n.startswith("_")}


def test_top_level_names_match():
    tree = stub_tree()
    aliases = {"EventKind", "Event", "Side", "Level", "L2", "Snapshot"}
    assert stub_names(tree) - aliases == module_public_names()


def test_book_members_match():
    tree = stub_tree()
    assert stub_class_members(tree, "Book") == class_public_members(lobcore.Book)
    assert stub_class_members(tree, "RefBook") == class_public_members(lobcore.RefBook)
    assert lobcore.Book.__doc__ and lobcore.RefBook.__doc__


def stub_signature(fn: ast.FunctionDef) -> list[tuple[str, str | None]]:
    """(name, default source) per parameter in order, skipping `self`; `...` defaults (a
    keyword documented without its value) are recorded as "..."."""
    a = fn.args
    positional = a.posonlyargs + a.args
    defaults = [None] * (len(positional) - len(a.defaults)) + [ast.unparse(d) for d in a.defaults]
    out = [(p.arg, d) for p, d in zip(positional, defaults, strict=True) if p.arg != "self"]
    out += [(p.arg, ast.unparse(d) if d is not None else None) for p, d in zip(a.kwonlyargs, a.kw_defaults, strict=True)]
    if a.vararg:
        out.append(("*" + a.vararg.arg, None))
    if a.kwarg:
        out.append(("**" + a.kwarg.arg, None))
    return out


def native_signature(obj) -> list[tuple[str, str | None]]:
    sig = inspect.signature(obj)
    out = []
    for p in sig.parameters.values():
        if p.name == "self":
            continue
        name = {p.VAR_POSITIONAL: "*", p.VAR_KEYWORD: "**"}.get(p.kind, "") + p.name
        out.append((name, None if p.default is p.empty else repr(p.default)))
    return out


def test_function_signatures_match_the_native_text_signatures():
    tree = stub_tree()
    checked = 0
    for node in tree.body:
        if isinstance(node, ast.FunctionDef):
            native = native_signature(getattr(lobcore, node.name))
            stub = stub_signature(node)
            # synth_itch documents the `**cfg` keys as keyword-only parameters with `...` defaults
            if node.name == "synth_itch":
                stub = [p for p in stub if p[1] != "..."] + [("**cfg", None)]
            assert stub == native, node.name
            checked += 1
        elif isinstance(node, ast.ClassDef) and node.name in ("Book", "RefBook"):
            cls = getattr(lobcore, node.name)
            for m in node.body:
                if not isinstance(m, ast.FunctionDef) or any(
                    isinstance(d, ast.Name) and d.id == "property" for d in m.decorator_list
                ):
                    continue
                target = cls if m.name == "__init__" else getattr(cls, m.name)
                assert stub_signature(m) == native_signature(target), f"{node.name}.{m.name}"
                checked += 1
    assert checked >= 6 + 2 * 12
