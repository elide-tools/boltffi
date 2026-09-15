//! Java spellings for C ABI types inside `@CFunction` declarations.
//!
//! The Native Image C API maps C scalars to Java primitives and every pointer
//! to a `PointerBase` word. Structs passed or returned by value have no Java
//! spelling, so callers must route them through the pointer-ABI shim first.

use crate::{
    bridge::{c, jni::JniType},
    core::{Error, Result},
};

use super::super::NATIVE_IMAGE_BRIDGE;

/// A Java type usable in a `@CFunction` signature.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum JavaCType {
    /// `void`.
    Void,
    /// `boolean`.
    Boolean,
    /// `byte`.
    Byte,
    /// `short`.
    Short,
    /// `int`.
    Int,
    /// `long`.
    Long,
    /// `float`.
    Float,
    /// `double`.
    Double,
    /// Any C pointer, spelled `org.graalvm.word.PointerBase`.
    Pointer,
}

impl JavaCType {
    /// Returns whether a C type is a struct passed or returned by value.
    pub fn is_by_value_struct(ty: &c::Type) -> bool {
        matches!(
            ty,
            c::Type::Status
                | c::Type::Buffer
                | c::Type::String
                | c::Type::Span
                | c::Type::CallbackHandle(_)
                | c::Type::Named(_)
                | c::Type::DirectRecord(_)
        )
    }

    /// Maps a C ABI type to its `@CFunction` Java spelling.
    pub fn from_c(ty: &c::Type) -> Result<Self> {
        Ok(match ty {
            c::Type::Void => Self::Void,
            c::Type::Bool => Self::Boolean,
            c::Type::Int8 | c::Type::Uint8 | c::Type::StreamPollResult => Self::Byte,
            c::Type::Int16 | c::Type::Uint16 => Self::Short,
            c::Type::Int32 | c::Type::Uint32 | c::Type::WaitResult => Self::Int,
            c::Type::Int64
            | c::Type::Uint64
            | c::Type::SignedPointerWidth
            | c::Type::PointerWidth => Self::Long,
            c::Type::Float32 => Self::Float,
            c::Type::Float64 => Self::Double,
            c::Type::FutureHandle
            | c::Type::ConstPointer(_)
            | c::Type::MutPointer(_)
            | c::Type::FunctionPointer { .. } => Self::Pointer,
            c::Type::CStyleEnum { repr, .. } => Self::from_c(repr)?,
            other if Self::is_by_value_struct(other) => {
                return Err(Error::UnsupportedTarget {
                    target: NATIVE_IMAGE_BRIDGE,
                    shape: "by-value struct in @CFunction signature",
                });
            }
            _ => {
                return Err(Error::UnsupportedTarget {
                    target: NATIVE_IMAGE_BRIDGE,
                    shape: "unknown C type",
                });
            }
        })
    }

    /// Maps a JNI primitive to the same Java primitive.
    pub fn from_jni(ty: JniType) -> Self {
        match ty {
            JniType::Boolean => Self::Boolean,
            JniType::Byte => Self::Byte,
            JniType::Short => Self::Short,
            JniType::Int => Self::Int,
            JniType::Long => Self::Long,
            JniType::Float => Self::Float,
            JniType::Double => Self::Double,
        }
    }

    /// Returns the Java source spelling.
    pub fn java(self) -> &'static str {
        match self {
            Self::Void => "void",
            Self::Boolean => "boolean",
            Self::Byte => "byte",
            Self::Short => "short",
            Self::Int => "int",
            Self::Long => "long",
            Self::Float => "float",
            Self::Double => "double",
            Self::Pointer => "org.graalvm.word.PointerBase",
        }
    }

    /// Returns whether this is `void`.
    pub const fn is_void(self) -> bool {
        matches!(self, Self::Void)
    }

    /// Returns the `@CEntryPointOptions` bailout class for a custom prologue.
    ///
    /// Native Image only derives a bailout value automatically for `void` and
    /// `int` entry points; every other return type must name a class whose
    /// `bailout(int)` supplies the value returned when the prologue fails.
    pub const fn prologue_bailout(self) -> Option<&'static str> {
        match self {
            Self::Void | Self::Int => None,
            Self::Boolean => Some("BailoutBoolean"),
            Self::Byte => Some("BailoutByte"),
            Self::Short => Some("BailoutShort"),
            Self::Long => Some("BailoutLong"),
            Self::Float => Some("BailoutFloat"),
            Self::Double => Some("BailoutDouble"),
            Self::Pointer => {
                Some("com.oracle.svm.guest.staging.c.function.CEntryPointOptions.ReturnNullPointer")
            }
        }
    }
}

impl std::fmt::Display for JavaCType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.java())
    }
}
