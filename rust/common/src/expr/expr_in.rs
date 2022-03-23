// Copyright 2022 Singularity Data
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::HashSet;
use std::fmt::Debug;
use std::sync::Arc;

use itertools::Itertools;

use crate::array::{ArrayBuilder, ArrayRef, BoolArrayBuilder, DataChunk};
use crate::expr::{BoxedExpression, Expression};
use crate::types::{DataType, Datum, ToOwnedDatum};

#[derive(Debug)]
pub(crate) struct InExpression {
    input_ref: BoxedExpression,
    set: HashSet<Datum>,
    return_type: DataType,
}

impl InExpression {
    pub fn new(
        input_ref: BoxedExpression,
        data: impl Iterator<Item = Datum>,
        return_type: DataType,
    ) -> Self {
        let mut sarg = HashSet::new();
        for datum in data {
            sarg.insert(datum);
        }
        Self {
            input_ref,
            set: sarg,
            return_type,
        }
    }

    fn exists(&self, datum: &Datum) -> bool {
        self.set.contains(datum)
    }
}

impl Expression for InExpression {
    fn return_type(&self) -> DataType {
        self.return_type.clone()
    }

    fn eval(&self, input: &DataChunk) -> crate::error::Result<ArrayRef> {
        let input_array = self.input_ref.eval(input)?;
        let visibility = input.visibility();
        let mut output_array = BoolArrayBuilder::new(input.cardinality())?;
        match visibility {
            Some(bitmap) => {
                for (data, vis) in input_array.iter().zip_eq(bitmap.iter()) {
                    if !vis {
                        continue;
                    }
                    let ret = self.exists(&data.to_owned_datum());
                    output_array.append(Some(ret))?;
                }
            }
            None => {
                for data in input_array.iter() {
                    let ret = self.exists(&data.to_owned_datum());
                    output_array.append(Some(ret))?;
                }
            }
        };
        Ok(Arc::new(output_array.finish()?.into()))
    }
}

#[cfg(test)]
mod tests {
    use crate::array::{DataChunk, Utf8Array};
    use crate::column;
    use crate::expr::expr_in::InExpression;
    use crate::expr::{Expression, InputRefExpression};
    use crate::types::{DataType, ScalarImpl};

    #[test]
    fn test_search_expr() {
        let input_ref = Box::new(InputRefExpression::new(DataType::Char, 0));
        let data = vec![
            Some(ScalarImpl::Utf8("abc".to_string())),
            Some(ScalarImpl::Utf8("def".to_string())),
        ];
        let search_expr = InExpression::new(input_ref, data.into_iter(), DataType::Boolean);
        let column = column! {Utf8Array, [Some("abc"), Some("a"), Some("def"), Some("abc")]};
        let data_chunk = DataChunk::builder().columns(vec![column]).build();
        let res = search_expr.eval(&data_chunk).unwrap();
        assert_eq!(res.datum_at(0), Some(ScalarImpl::Bool(true)));
        assert_eq!(res.datum_at(1), Some(ScalarImpl::Bool(false)));
        assert_eq!(res.datum_at(2), Some(ScalarImpl::Bool(true)));
        assert_eq!(res.datum_at(3), Some(ScalarImpl::Bool(true)));
    }
}
