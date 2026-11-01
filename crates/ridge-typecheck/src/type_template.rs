//! Source type templates resolved in their declaring module's scope.

use ridge_ast::{Item, Module, Type as AstType, TypeBody};
use ridge_types::{BuiltinTyCons, CapabilitySet, RowTail, TyConDecl, TyConId, Type};
use rustc_hash::{FxHashMap, FxHashSet};

pub struct TypeTemplates<'a> {
    pub modules: &'a [(u32, &'a Module)],
    pub scopes: &'a [FxHashMap<String, TyConId>],
    pub tycons: &'a [TyConDecl],
    pub builtins: &'a BuiltinTyCons,
}

impl TypeTemplates<'_> {
    fn lookup(&self, module: u32, name: &str) -> Option<TyConId> {
        crate::prelude::lookup_prelude_tycon(self.builtins, name)
            .or_else(|| {
                self.scopes
                    .get(module as usize)
                    .and_then(|names| names.get(name))
                    .copied()
            })
            .or_else(|| {
                self.tycons
                    .iter()
                    .find(|decl| {
                        decl.def_module_raw.is_none_or(|module| module == u32::MAX)
                            && decl.name == name
                    })
                    .map(|decl| decl.id)
            })
    }

    pub(crate) fn declaration(&self, module: u32, tycon: TyConId) -> Option<&ridge_ast::TypeDecl> {
        let ast = self.modules.iter().find(|(id, _)| *id == module)?.1;
        let names = self.scopes.get(module as usize)?;
        ast.items.iter().find_map(|item| match item {
            Item::Type(decl) if names.get(&decl.name.text) == Some(&tycon) => Some(decl),
            _ => None,
        })
    }

    fn alias(&self, id: TyConId) -> Option<(u32, &ridge_ast::TypeDecl, &AstType)> {
        let module = self.tycons.get(id.0 as usize)?.def_module_raw?;
        let decl = self.declaration(module, id)?;
        let TypeBody::Alias(body) = &decl.body else {
            return None;
        };
        Some((module, decl, body))
    }

    pub(crate) fn convert(
        &self,
        module: u32,
        ty: &AstType,
        params: &FxHashMap<String, Type>,
        aliases: &mut FxHashSet<TyConId>,
    ) -> Option<Type> {
        self.convert_inner(module, ty, params, aliases, false)
    }

    pub(crate) fn convert_head(
        &self,
        module: u32,
        ty: &AstType,
        params: &FxHashMap<String, Type>,
    ) -> Option<Type> {
        self.convert_inner(module, ty, params, &mut FxHashSet::default(), true)
    }

    fn convert_inner(
        &self,
        module: u32,
        ty: &AstType,
        params: &FxHashMap<String, Type>,
        aliases: &mut FxHashSet<TyConId>,
        open_rows: bool,
    ) -> Option<Type> {
        match ty {
            AstType::Var { name, .. } => params.get(&name.text).cloned(),
            AstType::Paren { inner, .. } => {
                self.convert_inner(module, inner, params, aliases, open_rows)
            }
            AstType::Primitive { name, .. } => {
                let id = self
                    .tycons
                    .iter()
                    .find(|decl| decl.name == name.name() && decl.def_module_raw.is_none())?
                    .id;
                Some(Type::Con(id, vec![]))
            }
            AstType::Named { name, .. } => {
                if let Some(ty) = params.get(&name.text) {
                    return Some(ty.clone());
                }
                self.applied(
                    module,
                    self.lookup(module, &name.text)?,
                    vec![],
                    aliases,
                    open_rows,
                )
            }
            AstType::App { head, args, .. } => {
                let args = args
                    .iter()
                    .map(|arg| self.convert_inner(module, arg, params, aliases, open_rows))
                    .collect::<Option<Vec<_>>>()?;
                self.applied(
                    module,
                    self.lookup(module, &head.text)?,
                    args,
                    aliases,
                    open_rows,
                )
            }
            AstType::List { elem, .. } => Some(Type::Con(
                self.builtins.list,
                vec![self.convert_inner(module, elem, params, aliases, open_rows)?],
            )),
            AstType::Tuple { elems, .. } => Some(Type::Tuple(
                elems
                    .iter()
                    .map(|elem| self.convert_inner(module, elem, params, aliases, open_rows))
                    .collect::<Option<Vec<_>>>()?,
            )),
            AstType::Record {
                fields, tail: None, ..
            } => Some(Type::record(
                fields
                    .iter()
                    .map(|field| {
                        Some((
                            field.name.text.clone(),
                            self.convert_inner(module, &field.ty, params, aliases, open_rows)?,
                        ))
                    })
                    .collect::<Option<Vec<_>>>()?,
                RowTail::Closed,
            )),
            AstType::Record {
                fields,
                tail: Some(tail),
                ..
            } => {
                if !open_rows {
                    return None;
                }
                let Type::Var(var) = params.get(&tail.text)? else {
                    return None;
                };
                Some(Type::record(
                    fields
                        .iter()
                        .map(|field| {
                            Some((
                                field.name.text.clone(),
                                self.convert_inner(module, &field.ty, params, aliases, open_rows)?,
                            ))
                        })
                        .collect::<Option<Vec<_>>>()?,
                    RowTail::Open(ridge_types::RowVid(var.0)),
                ))
            }
            AstType::Fn { fn_ty, .. } => Some(Type::Fn {
                params: fn_ty
                    .params
                    .iter()
                    .map(|ty| self.convert_inner(module, ty, params, aliases, open_rows))
                    .collect::<Option<Vec<_>>>()?,
                ret: Box::new(self.convert_inner(module, &fn_ty.ret, params, aliases, open_rows)?),
                caps: ridge_types::CapRow::Concrete(
                    fn_ty.caps.iter().fold(CapabilitySet::PURE, |caps, cap| {
                        caps.union(&CapabilitySet::singleton(*cap))
                    }),
                ),
            }),
        }
    }

    fn applied(
        &self,
        _module: u32,
        id: TyConId,
        args: Vec<Type>,
        aliases: &mut FxHashSet<TyConId>,
        open_rows: bool,
    ) -> Option<Type> {
        let Some((module, decl, body)) = self.alias(id) else {
            return Some(crate::tycon_collect::chase_alias_chain(
                self.tycons,
                &Type::Con(id, args),
                &mut vec![],
            ));
        };
        if args.len() != decl.params.len() || !aliases.insert(id) {
            return None;
        }
        let params = decl
            .params
            .iter()
            .zip(args)
            .map(|(param, ty)| (param.text.clone(), ty))
            .collect();
        let result = self.convert_inner(module, body, &params, aliases, open_rows);
        aliases.remove(&id);
        result.map(|body| Type::Alias {
            name: id,
            body: Box::new(body),
        })
    }
}
