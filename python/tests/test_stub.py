"""The hand-written stub `python/lobcore/lobcore.pyi` names exactly the public surface of the
extension module (top-level names, and every method / property of Book and RefBook)."""

from __future__ import annotations

import ast
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
