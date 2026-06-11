# Inventory: LLVM-C surface used by codegen

**Status:** DONE (initial pass, 2026-06-10). Resolves sub-question 1 (*Surface scope*) of
[Spike: LLVM-C FFI binding](self-hosting-llvm-c-ffi.md#open-sub-questions-resolve-before-the-codegen-port).
This is the call-site inventory the spike calls for: "that inventory IS the exact set of
`extern "C"` signatures to declare."

## Method

Mechanically extracted from every `inkwell` call site under `src/` (`src/codegen.rs` +
~50 `src/codegen/*.rs` submodules), then subtracted codegen's own `build_*` / `const_*` /
`*_type` **helper** methods (those are higher-level Kāra-codegen functions that internally
call the primitive inkwell ops — they need no extern). Counts are call-site frequencies — a
proxy for how load-bearing each entry is, not a requirement to declare more than one extern.

**Headline:** codegen uses **~120 distinct llvm-c functions**, out of the many hundreds
`inkwell` wraps. That is the entire `extern "C"` surface the self-hosted codegen must declare.

## Finding that changes the handle model (feeds sub-question 3)

The single largest "inkwell surface" by call count — **~1,900 sites** of
`.into_int_value()`, `.into_pointer_value()`, `.into_struct_value()`, `.into_float_value()`,
`.into_vector_value()`, `.into_array_value()`, `.as_pointer_value()`, `.as_basic_value_enum()`
— maps to **zero llvm-c functions**. These exist only because inkwell models values as a
*typed* Rust enum (`BasicValueEnum`) and forces coercion between variants. Raw llvm-c has a
single untyped `LLVMValueRef`; all of these coercions **disappear** in the Kāra binding.

Consequence: the Kāra handle model (sub-q 3) is *simpler* than inkwell's, not harder. One
opaque `LLVMValueRef` newtype replaces inkwell's whole typed-value enum hierarchy. The
~1,900 coercion calls become nothing. Same for `into_int_type` / `into_struct_type` /
`into_float_type` on the type side. `.as_value_ref()` (7 sites) is the telling one — it
*already* drops to the raw `LLVMValueRef`, i.e. the C handle the Kāra binding uses everywhere.

## Version pin (feeds sub-question 2)

inkwell on **LLVM 18** uses the opaque-pointer-era "2" variants — `LLVMBuildLoad2`,
`LLVMBuildCall2`, `LLVMBuildGEP2`, `LLVMBuildStructGEP2`, `LLVMBuildInBoundsGEP2`,
`LLVMArrayType2` — which take an *explicit* element/function type (the pre-opaque-pointer
`LLVMBuildLoad`/`LLVMBuildCall`/`LLVMBuildGEP` are gone). The Kāra binding **must** declare
the `2` variants and thread element types through GEP/load/call, or it will not link against
LLVM 18 and IR will drift. Pin LLVM 18 across stage-1 (Rust karac) and stage-2+ (self-hosted).

## Composite ops (not 1:1 — the binding must replicate the composition)

A few inkwell conveniences are **not** single C entry points; inkwell builds them from
primitives. The Kāra binding must replicate the composition, not look for a missing C function:

| inkwell method | sites | actually emitted as |
|---|---|---|
| `build_memcpy` | 50 | intrinsic lookup (`llvm.memcpy.*`) + `LLVMBuildCall2` |
| `build_memmove` | 5 | intrinsic lookup (`llvm.memmove.*`) + `LLVMBuildCall2` |
| `build_memset` | 3 | intrinsic lookup (`llvm.memset.*`) + `LLVMBuildCall2` |
| `build_global_string_ptr` | 37 | `LLVMBuildGlobalString` then GEP to i8* (one C call exists: `LLVMBuildGlobalStringPtr`) |
| `const_zero` | 202 | `LLVMConstNull` of the type |

---

## The enumerated surface

### 1. IRBuilder — instruction emission (~62 fns)

| inkwell | sites | llvm-c |
|---|---|---|
| build_load | 624 | LLVMBuildLoad2 |
| build_store | 548 | LLVMBuildStore |
| build_call / build_indirect_call | 469 | LLVMBuildCall2 |
| build_struct_gep | 393 | LLVMBuildStructGEP2 |
| build_unconditional_branch | 363 | LLVMBuildBr |
| build_int_compare | 340 | LLVMBuildICmp |
| build_extract_value | 280 | LLVMBuildExtractValue |
| build_conditional_branch | 278 | LLVMBuildCondBr |
| build_insert_value | 246 | LLVMBuildInsertValue |
| build_return | 125 | LLVMBuildRet / LLVMBuildRetVoid |
| build_int_mul | 121 | LLVMBuildMul |
| build_int_add | 121 | LLVMBuildAdd |
| build_gep | 113 | LLVMBuildGEP2 |
| build_select | 89 | LLVMBuildSelect |
| build_in_bounds_gep | 78 | LLVMBuildInBoundsGEP2 |
| build_phi | 60 | LLVMBuildPhi |
| build_memcpy | 50 | *(composite — see above)* |
| build_unreachable | 45 | LLVMBuildUnreachable |
| build_int_sub | 45 | LLVMBuildSub |
| build_int_z_extend | 39 | LLVMBuildZExt |
| build_int_to_ptr | 39 | LLVMBuildIntToPtr |
| build_global_string_ptr | 37 | LLVMBuildGlobalStringPtr |
| build_int_truncate | 35 | LLVMBuildTrunc |
| build_ptr_to_int | 33 | LLVMBuildPtrToInt |
| build_is_null | 31 | LLVMBuildIsNull |
| build_and | 23 | LLVMBuildAnd |
| build_alloca | 17 | LLVMBuildAlloca |
| build_not | 15 | LLVMBuildNot |
| build_float_compare | 15 | LLVMBuildFCmp |
| build_or | 14 | LLVMBuildOr |
| build_switch | 11 | LLVMBuildSwitch |
| build_insert_element | 11 | LLVMBuildInsertElement |
| build_int_s_extend | 10 | LLVMBuildSExt |
| build_xor | 8 | LLVMBuildXor |
| build_int_z_extend_or_bit_cast | 7 | LLVMBuildZExtOrBitCast |
| build_extract_element | 7 | LLVMBuildExtractElement |
| build_bit_cast | 7 | LLVMBuildBitCast |
| build_unsigned_int_to_float | 5 | LLVMBuildUIToFP |
| build_memmove | 5 | *(composite)* |
| build_int_unsigned_rem | 5 | LLVMBuildURem |
| build_int_unsigned_div | 5 | LLVMBuildUDiv |
| build_float_div | 5 | LLVMBuildFDiv |
| build_atomicrmw | 5 | LLVMBuildAtomicRMW |
| build_right_shift | 4 | LLVMBuildLShr / LLVMBuildAShr |
| build_pointer_cast | 4 | LLVMBuildPointerCast |
| build_left_shift | 4 | LLVMBuildShl |
| build_float_cast | 4 | LLVMBuildFPCast |
| build_signed_int_to_float | 3 | LLVMBuildSIToFP |
| build_memset | 3 | *(composite)* |
| build_int_signed_rem | 3 | LLVMBuildSRem |
| build_int_signed_div | 3 | LLVMBuildSDiv |
| build_float_ext | 3 | LLVMBuildFPExt |
| build_float_add | 3 | LLVMBuildFAdd |
| build_int_nsw_sub | 2 | LLVMBuildNSWSub |
| build_float_sub | 2 | LLVMBuildFSub |
| build_float_rem | 2 | LLVMBuildFRem |
| build_float_neg | 2 | LLVMBuildFNeg |
| build_float_mul | 2 | LLVMBuildFMul |
| build_cmpxchg | 2 | LLVMBuildAtomicCmpXchg |
| build_is_not_null | 1 | LLVMBuildIsNotNull |
| build_int_nsw_mul | 1 | LLVMBuildNSWMul |
| build_int_nsw_add | 1 | LLVMBuildNSWAdd |

Builder lifecycle: `create_builder` → LLVMCreateBuilderInContext; `position_at_end` (872) →
LLVMPositionBuilderAtEnd; `position_before` (4) → LLVMPositionBuilderBefore; `get_insert_block`
(193) → LLVMGetInsertBlock.

### 2. Constants (~10 fns)

| inkwell | sites | llvm-c |
|---|---|---|
| const_int | 782 | LLVMConstInt |
| const_zero | 202 | LLVMConstNull *(composite)* |
| const_null | 70 | LLVMConstNull / LLVMConstPointerNull |
| const_array | 15 | LLVMConstArray2 |
| const_named_struct | 5 | LLVMConstNamedStruct |
| const_float | 5 | LLVMConstReal |
| const_string | 3 | LLVMConstStringInContext |
| const_struct | 2 | LLVMConstStructInContext |
| const_all_ones | 2 | LLVMConstAllOnes |
| const_to_pointer | 1 | LLVMConstIntToPtr |
| const_shl | 1 | LLVMConstShl |

### 3. Types (~16 fns)

| inkwell | sites | llvm-c |
|---|---|---|
| i64/i32/i16/i8/i128_type | 673 | LLVMInt{64,32,16,8,128}TypeInContext |
| fn_type | 274 | LLVMFunctionType |
| ptr_type | 237 | LLVMPointerType (opaque ptr on LLVM 18) |
| void_type | 102 | LLVMVoidTypeInContext |
| struct_type | 102 | LLVMStructTypeInContext |
| bool_type | 50 | LLVMInt1TypeInContext |
| array_type | 39 | LLVMArrayType2 |
| f64/f32_type | 32 | LLVMDoubleTypeInContext / LLVMFloatTypeInContext |
| get_element_type | 20 | LLVMGetElementType |
| get_return_type | 11 | LLVMGetReturnType |
| custom_width_int_type | 1 | LLVMIntTypeInContext |
| opaque_struct_type | 3 | LLVMStructCreateNamed |
| (llvm_)vector_type | 8 | LLVMVectorType |
| get_type *(on value)* | 202 | LLVMTypeOf |

`into_int_type` / `into_struct_type` / `into_float_type` → **no C call** (inkwell enum coercion).

### 4. Module / function / global / basic block (~22 fns)

| inkwell | sites | llvm-c |
|---|---|---|
| append_basic_block | 797 | LLVMAppendBasicBlockInContext |
| get_function / get_named_function | 254 | LLVMGetNamedFunction |
| add_function | 251 | LLVMAddFunction |
| get_nth_param | 122 | LLVMGetParam |
| add_incoming | 77 | LLVMAddIncoming |
| set_linkage | 26 | LLVMSetLinkage |
| set_initializer | 24 | LLVMSetInitializer |
| add_global | 20 | LLVMAddGlobal |
| print_to_string | 13 | LLVMPrintModuleToString |
| verify | 8 | LLVMVerifyModule |
| add_attribute | 8 | LLVMAddAttributeAtIndex |
| create_string_attribute | 6 | LLVMCreateStringAttribute |
| get_first_basic_block | 6 | LLVMGetFirstBasicBlock |
| set_alignment | 5 | LLVMSetAlignment |
| create_module | 5 | LLVMModuleCreateWithNameInContext |
| get_global | 4 | LLVMGetNamedGlobal |
| set_section | 3 | LLVMSetSection |
| create_enum_attribute | 2 | LLVMCreateEnumAttribute |
| set_current_debug_location | 1 | LLVMSetCurrentDebugLocation2 |

`as_global_value` (38) → **no C call** (inkwell cast). Context accessor → LLVMContextCreate /
LLVMGetModuleContext.

### 5. Target machine + object emit (~12 fns)

| inkwell | sites | llvm-c |
|---|---|---|
| Target::initialize_native | 7 | *(see note)* LLVMInitialize{Arch}TargetInfo/Target/TargetMC/AsmPrinter |
| Target::initialize_webassembly | 2 | LLVMInitializeWebAssembly{TargetInfo,Target,TargetMC,AsmPrinter} |
| Target::from_triple | 3 | LLVMGetTargetFromTriple |
| TargetMachine::get_default_triple | 1 | LLVMGetDefaultTargetTriple |
| create_target_machine | 3 | LLVMCreateTargetMachine |
| get_target_data | 5 | LLVMCreateTargetDataLayout |
| set_triple | 3 | LLVMSetTarget |
| set_data_layout | 3 | LLVMSetModule DataLayout (LLVMSetDataLayout) |
| write_to_file | 3 | LLVMTargetMachineEmitToFile |
| run_passes | 6 | LLVMRunPasses *(new pass-manager C API)* |
| (memory buffer) as_slice | 49 | LLVMGetBufferStart / LLVMGetBufferSize |

### 6. Intrinsics + debug info (~7 fns)

| inkwell | sites | llvm-c |
|---|---|---|
| Intrinsic::find | 4 | LLVMLookupIntrinsicID |
| get_declaration | 4 | LLVMGetIntrinsicDeclaration |
| create_debug_info_builder | 1 | LLVMCreateDIBuilder |
| create_function | 1 | LLVMDIBuilderCreateFunction |
| create_compile_unit | — | LLVMDIBuilderCreateCompileUnit |
| create_subroutine_type | 1 | LLVMDIBuilderCreateSubroutineType |
| finalize | 1 | LLVMDIBuilderFinalize |

**Note — `initialize_native` is not a single symbol.** `LLVMInitializeNativeTarget` /
`LLVMInitializeNativeAsmPrinter` are `static inline` in `llvm-c/Target.h` (they expand to the
host-arch concrete initializers), **not** exported `libLLVM` symbols — so the Kāra binding cannot
`extern "C"` them. Declare and call the concrete per-arch quartet instead:
`LLVMInitializeAArch64{TargetInfo,Target,TargetMC,AsmPrinter}` on Apple Silicon,
`LLVMInitializeX86{…}` on x86-64. Surfaced while writing the [minimal proof](self-hosting-llvm-c-proof.md).

JIT (`create_jit_execution_engine`, 1 site) is the lljit path — out of scope for the AOT
codegen port per the spike; track separately if self-hosted REPL/JIT is in Phase 12 scope.

---

## What this unblocks

- **Sub-q 2 (linking):** pin LLVM 18; the `2`-suffixed opaque-pointer functions above are the
  hard ABI constraint.
- **Sub-q 3 (handles):** opaque newtypes needed for `LLVMValueRef`, `LLVMTypeRef`,
  `LLVMModuleRef`, `LLVMBuilderRef`, `LLVMBasicBlockRef`, `LLVMContextRef`,
  `LLVMTargetMachineRef`, `LLVMTargetDataRef`, `LLVMMemoryBufferRef`, `LLVMDIBuilderRef`,
  `LLVMValueMetadataEntry`/attribute refs. Owned-with-Drop: Context, Builder, Module,
  TargetMachine, MemoryBuffer, DIBuilder (each has an `LLVMDispose*`). The rest are
  non-owning (owned by the module/context). The ~1,900 typed-value coercions collapse to nothing.
- **Minimal proof (DoD):** the smallest end-to-end slice touches exactly — create context/module/
  builder, int + fn + void types, one function + entry block, const_int + add + ret, verify,
  create target machine, emit object to file. ~20 of the functions above.
