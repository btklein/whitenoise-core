use whitenoise_validator::errors::*;

use ndarray::prelude::*;
use crate::NodeArguments;
use whitenoise_validator::base::{Value, Array, Hashmap, ReleaseNode};
use crate::components::Evaluable;
use std::collections::BTreeMap;
use ndarray;
use whitenoise_validator::proto;
use whitenoise_validator::utilities::serial::parse_value;
use whitenoise_validator::utilities::array::{slow_stack, slow_select};
use std::cmp::Ordering;
use whitenoise_validator::utilities::get_ith_column;

impl Evaluable for proto::Materialize {
    fn evaluate(&self, arguments: &NodeArguments) -> Result<ReleaseNode> {
        let column_names = arguments.get("column_names")
            .and_then(|column_names| column_names.array().ok()?.string().ok()).cloned();

        let num_columns = arguments.get("num_columns")
            .and_then(|num_columns| num_columns.first_i64().ok());

        // num columns is sufficient shared information to build the dataframes
        let num_columns = match (column_names.clone(), num_columns) {
            (Some(column_names), None) => match column_names.into_dimensionality::<Ix1>() {
                Ok(column_names) => column_names,
                Err(_) => return Err("column names must be one-dimensional".into())
            }.to_vec().len(),
            (None, Some(num_columns)) => num_columns as usize,
            _ => return Err("either column_names or num_columns must be provided".into())
        };

        let data_source = self.data_source.clone()
            .ok_or_else(|| Error::from("data source must be supplied"))?;

        match data_source.value.as_ref().unwrap() {
            proto::data_source::Value::Literal(value) => {
                // force the input to be an array- reject hashmap and jagged
                Ok(ReleaseNode::new(match column_names {
                    Some(column_names) => Value::Hashmap(Hashmap::<Value>::Str(match parse_value(value)?.array()? {
                        Array::F64(array) => {
                            let standardized = standardize_columns(array, num_columns)?;
                            column_names.into_iter().enumerate()
                                .map(|(idx, name)| Ok((name.clone(), get_ith_column(&standardized, &idx)?.into())))
                                .collect::<Result<BTreeMap<String, Value>>>()?
                        }
                        Array::I64(array) => {
                            let standardized = standardize_columns(array, num_columns)?;
                            column_names.into_iter().enumerate()
                                .map(|(idx, name)| Ok((name.clone(), get_ith_column(&standardized, &idx)?.into())))
                                .collect::<Result<BTreeMap<String, Value>>>()?
                        }
                        Array::Bool(array) => {
                            let standardized = standardize_columns(array, num_columns)?;
                            column_names.into_iter().enumerate()
                                .map(|(idx, name)| Ok((name.clone(), get_ith_column(&standardized, &idx)?.into())))
                                .collect::<Result<BTreeMap<String, Value>>>()?
                        }
                        Array::Str(array) => {
                            let standardized = standardize_columns(array, num_columns)?;
                            column_names.into_iter().enumerate()
                                .map(|(idx, name)| Ok((name.clone(), get_ith_column(&standardized, &idx)?.into())))
                                .collect::<Result<BTreeMap<String, Value>>>()?
                        }
                    })),
                    None => match parse_value(value)?.array()? {
                        Array::F64(array) => standardize_columns(array, num_columns)?.into(),
                        Array::I64(array) => standardize_columns(array, num_columns)?.into(),
                        Array::Bool(array) => standardize_columns(array, num_columns)?.into(),
                        Array::Str(array) => standardize_columns(array, num_columns)?.into(),
                    }
                }))
            }
            proto::data_source::Value::FilePath(path) => {
                let mut response = (0..num_columns)
                    .map(|_| Vec::new())
                    .collect::<Vec<Vec<String>>>();

                let mut reader = match csv::ReaderBuilder::new()
                    .has_headers(self.skip_row)
                    .from_path(path) {
                    Ok(reader) => reader,
                    Err(_) => return Err("provided file path could not be found".into())
                };

                // parse from csv into response
                reader.deserialize().map(|result: std::result::Result<Vec<String>, _>| {

                    // parse each record into the whitenoise internal format
                    match result {
                        Ok(record) => record.into_iter().enumerate()
                            .filter(|(idx, _)| idx < &num_columns)
                            .for_each(|(idx, value)| response[idx].push(value)),
                        Err(e) => return Err(format!("{:?}", e).into())
                    };
                    Ok(())
                }).collect::<Result<()>>()?;

                let num_nonempty_columns = response.iter()
                    .filter(|col| col.len() > 0).count();

                if 0 < num_nonempty_columns && num_nonempty_columns < num_columns {
                    (num_nonempty_columns..num_columns).for_each(|idx|
                        response[idx] = (0..response[0].len()).map(|_| "".to_string()).collect::<Vec<String>>())
                }

                match column_names {
                    Some(column_names) => {
                        let column_names = column_names.into_dimensionality::<Ix1>()?.to_vec();
                        // convert hashmap of vecs into arrays
                        Ok(ReleaseNode::new(Value::Hashmap(Hashmap::Str(response.into_iter().enumerate()
                            .map(|(k, v): (usize, Vec<String>)|
                                (column_names[k].clone(), ndarray::Array::from(v).into_dyn().into()))
                            .collect::<BTreeMap<String, Value>>()))))
                    }
                    None => {

                        // convert hashmap of vecs into arrays
                        Ok(ReleaseNode::new(Value::Hashmap(Hashmap::I64(response.into_iter().enumerate()
                            .map(|(k, v): (usize, Vec<String>)|
                                (k as i64, ndarray::Array::from(v).into_dyn().into()))
                            .collect::<BTreeMap<i64, Value>>()))))
                    }
                }
            }
            _ => Err("the selected table reference format is not implemented".into())
        }
    }
}


fn standardize_columns<T: Default + Clone>(array: &ArrayD<T>, column_len: usize) -> Result<ArrayD<T>> {
    Ok(match array.ndim() {
        0 => return Err("dataset may not be a scalar".into()),
        1 => match column_len {
            0 => slow_select(array, Axis(1), &[]),
            1 => array.clone(),
            _ => slow_stack(
                Axis(1),
                &[array.view(), ndarray::Array::<T, IxDyn>::default(IxDyn(&[array.len(), column_len])).view()])?
        },
        2 => match array.len_of(Axis(1)).cmp(&column_len) {
            Ordering::Less => slow_stack(
                Axis(1),
                &[array.view(), ndarray::Array::<T, IxDyn>::default(IxDyn(&[
                    array.len_of(Axis(0)),
                    column_len - array.len_of(Axis(1))])).view()],
            )?,
            Ordering::Equal => array.clone(),
            Ordering::Greater => slow_select(array, Axis(1), &(0..column_len).collect::<Vec<_>>())
        },
        _ => return Err("array must be 1 or 2-dimensional".into())
    })
}