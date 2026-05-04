# The Kāra Programming Language

Welcome to The Kāra Book — the official guide to learning the Kāra programming language.

## What is Kāra?

Kāra is a systems programming language where you declare *what* and *why*, and the compiler decides *how*. You write sequential-looking code with clear intent, and the compiler infers borrow lifetimes, optimizes memory layout, and parallelizes independent work — without explicit annotations.

The language is built on four layers, in order of importance:

1. **Values and types** define structure.
2. **Effects** define observable behavior.
3. **Ownership** defines aliasing and lifetimes.
4. **Layout** defines physical memory representation.

If you're coming from Rust, think of Kāra as a language that shares many of the same safety goals but takes a different path — owned by default with explicit `ref` / `mut ref` modes (no `<'a>` lifetime parameters), and using an effect system to track side effects and enable parallelization of independent work.

If you're coming from Python, Go, or TypeScript, Kāra will feel familiar in syntax while giving you performance and safety guarantees that those languages can't offer.

## What this book covers

This book walks through the language from first principles. You don't need prior systems programming experience, though familiarity with any typed language will help.

- **Getting Started** covers the basics: variables, functions, control flow.
- **Core Concepts** introduces structs, enums, pattern matching, error handling, traits, and generics.
- **What Makes Kāra Different** digs into the features that set Kāra apart: the effect system, ownership without lifetime annotations, and the module system.
- **Advanced Topics** covers concurrency, data layout control, and testing.

Each chapter builds on the ones before it, with code examples you can run.

## Who is this for?

Anyone who wants to write fast, safe code without fighting the compiler. Whether you're building a web server, a CLI tool, an embedded system, or a data pipeline — Kāra is designed to get out of your way while keeping your programs correct.

Let's get started.
