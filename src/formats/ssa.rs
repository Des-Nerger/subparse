// This file is part of the Rust library `subparse`.
//
// Copyright (C) 2017 kaegi
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Lesser General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Lesser General Public License for more details.
//
// You should have received a copy of the GNU Lesser General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.


use {ParseSubtitleString, SubtitleEntry, SubtitleFile};
use errors::Result as SubtitleParserResult;
use formats::common::*;
use timetypes::{TimePoint, TimeSpan};
use self::errors::ErrorKind::*;
use self::errors::*;

use std::iter::once;

use combine::char::*;
use combine::combinator::*;
use combine::primitives::Parser;

/// `.ssa`-parser-specific errors
#[allow(missing_docs)]
pub mod errors {
    // see https://docs.rs/error-chain/0.8.1/error_chain/
    error_chain! {
        errors {
            SsaFieldsInfoNotFound {
                description(".ssa/.ass file did not have a line beginning with `Format: ` in a `[Events]` section")
            }
            SsaMissingField(line_num: usize, f: &'static str) {
                display("the '{}' field is missing in the field info in line {}", f, line_num)
            }
            SsaDuplicateField(line_num: usize, f: &'static str) {
                display("the '{}' field is twice in the field info in line {}", f, line_num)
            }
            SsaTextFieldNotLast(line_num: usize) {
                display("the field info in line {} has to have `Text` as its last field", line_num)
            }
            SsaIncorrectNumberOfFields(line_num: usize) {
                display("the dialog at line {} has incorrect number of fields", line_num)
            }
            SsaWrongTimepointFormat(line_num: usize, string: String) {
                display("the timepoint `{}` in line {} has wrong format", string, line_num)
            }
            SsaDialogLineParseError(line_num: usize, msg: String) {
                display("parsing the line `{}` failed because of `{}`", line_num, msg)
            }
            SsaLineParseError(line_num: usize, msg: String) {
                display("parsing the line `{}` failed because of `{}`", line_num, msg)
            }
        }
    }
}

// ////////////////////////////////////////////////////////////////////////////////////////////////
// SSA field info

struct SsaFieldsInfo {
    start_field_idx: usize,
    end_field_idx: usize,
    text_field_idx: usize,
    num_fields: usize,
}

impl SsaFieldsInfo {
    /// Parses a format line like "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text".
    fn new_from_fields_info_line(line_num: usize, s: String) -> Result<SsaFieldsInfo> {
        assert!(s.starts_with("Format:"));
        let field_info = &s["Format:".len()..];
        let mut start_field_idx: Option<usize> = None;
        let mut end_field_idx: Option<usize> = None;
        let mut text_field_idx: Option<usize> = None;

        // filter "Start" and "End" and "Text"
        let split_iter = field_info.split(',');
        let num_fields = split_iter.clone().count();
        for (i, field_name) in split_iter.enumerate() {
            let trimmed = field_name.trim();
            if trimmed == "Start" {
                if start_field_idx.is_some() {
                    return Err(SsaDuplicateField(line_num, "Start"))?;
                }
                start_field_idx = Some(i);
            } else if trimmed == "End" {
                if end_field_idx.is_some() {
                    return Err(SsaDuplicateField(line_num, "End"))?;
                }
                end_field_idx = Some(i);
            } else if trimmed == "Text" {
                if text_field_idx.is_some() {
                    return Err(SsaDuplicateField(line_num, "Text"))?;
                }
                text_field_idx = Some(i);
            }
        }

        let text_field_idx2 = text_field_idx.ok_or_else(|| Error::from(SsaMissingField(line_num, "Text")))?;
        if text_field_idx2 != num_fields - 1 {
            return Err(SsaTextFieldNotLast(line_num))?;
        }

        Ok(SsaFieldsInfo {
            start_field_idx: start_field_idx.ok_or_else(|| Error::from(SsaMissingField(line_num, "Start")))?,
            end_field_idx: end_field_idx.ok_or_else(|| Error::from(SsaMissingField(line_num, "End")))?,
            text_field_idx: text_field_idx2,
            num_fields: num_fields,
        })
    }
}


// ////////////////////////////////////////////////////////////////////////////////////////////////
// SSA parser

impl ParseSubtitleString for SsaFile {
    fn parse_from_string(s: String) -> SubtitleParserResult<SsaFile> {
        match Self::parse_inner(s) {
            Ok(v) => Ok(v),
            Err(e) => Err(e.into()),
        }
    }
}

/// Implement parser helper functions.
impl SsaFile {
    /// Parses a whole `.ssa` file from string.
    fn parse_inner(i: String) -> Result<SsaFile> {
        let mut file_parts = Vec::new();
        let (bom, s) = split_bom(&i);
        file_parts.push(SsaFilePart::Filler(bom.to_string()));

        // first we need to find and parse the format line, which then dictates how to parse the file
        let (line_num, field_info_line) = Self::get_format_info(s)?;
        let fields_info = SsaFieldsInfo::new_from_fields_info_line(line_num, field_info_line)?;

        // parse the dialog lines with the given format
        file_parts.append(&mut Self::parse_dialog_lines(&fields_info, s)?);
        Ok(SsaFile::new(file_parts))
    }

    /// Searches and parses a format line like "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text".
    fn get_format_info(s: &str) -> Result<(usize, String)> {
        let mut section_opt = None;
        for (line_num, line) in s.lines().enumerate() {
            // parse section headers like `[Events]`
            let trimmed_line = line.trim();
            if trimmed_line.starts_with('[') && trimmed_line.ends_with(']') {
                section_opt = Some(&trimmed_line[1..trimmed_line.len() - 1]);
            }

            // most sections have a format line, but we only want the one for the subtitle events
            if section_opt != Some("Events") {
                continue;
            }
            if !line.trim().starts_with("Format:") {
                continue;
            }
            return Ok((line_num, line.to_string()));
        }

        Err(SsaFieldsInfoNotFound.into())
    }

    /// Filters file for lines like this and parses them:
    ///
    /// ```text
    /// "Dialogue: 1,0:22:43.52,0:22:46.22,ED-Romaji,,0,0,0,,{\fad(150,150)\blur0.5\bord1}some text"
    /// ```
    fn parse_dialog_lines(fields_info: &SsaFieldsInfo, s: &str) -> Result<Vec<SsaFilePart>> {
        let mut result = Vec::new();
        let mut section_opt: Option<String> = None;
        let lines_with_newl: Vec<(String, String)> = get_lines_non_destructive(s)
            .map_err(|(line_num, err_str)| SsaLineParseError(line_num, err_str))?;

        for (line_num, (line, newl)) in lines_with_newl.into_iter().enumerate() {
            let trimmed_line = line.trim().to_string();

            // parse section headers like `[Events]`
            if trimmed_line.starts_with('[') && trimmed_line.ends_with(']') {
                section_opt = Some(trimmed_line[1..trimmed_line.len() - 1].to_string());
                result.push(SsaFilePart::Filler(line));
                result.push(SsaFilePart::Filler("\n".to_string()));
                continue;
            }

            if section_opt.is_none() || section_opt.iter().any(|s| s != "Events") || !trimmed_line.starts_with("Dialogue:") {
                result.push(SsaFilePart::Filler(line));
                result.push(SsaFilePart::Filler("\n".to_string()));
                continue;
            }

            result.append(&mut Self::parse_dialog_line(line_num, line.as_str(), fields_info)?);
            result.push(SsaFilePart::Filler(newl));
        }

        Ok(result)
    }

    /// Parse lines like:
    ///
    /// ```text
    /// "Dialogue: 1,0:22:43.52,0:22:46.22,ED-Romaji,,0,0,0,,{\fad(150,150)\blur0.5\bord1}some text"
    /// ```
    fn parse_dialog_line(line_num: usize, line: &str, fields_info: &SsaFieldsInfo) -> Result<Vec<SsaFilePart>> {
        let parts_res = (many(ws()),
                         string("Dialogue:"),
                         many(ws()),
                         count(fields_info.num_fields - 1,
                               (many(none_of(once(','))), token(','))),
                         many(try(any())))
                .map(|(ws1, dl, ws2, v, text): (String, &str, String, Vec<(String, char)>, String)| -> Result<Vec<SsaFilePart>> {
                    let mut result: Vec<SsaFilePart> = Vec::new();
                    result.push(SsaFilePart::Filler(ws1));
                    result.push(SsaFilePart::Filler(dl.to_string()));
                    result.push(SsaFilePart::Filler(ws2.to_string()));
                    result.append(&mut Self::parse_fields(line_num, fields_info, v)?);
                    result.push(SsaFilePart::Text(text));
                    Ok(result)
                })
                .parse(line);

        match parts_res {
            // Ok() means that parsing succeded, but the "map" function might created an SSA error
            Ok((parts, _)) => Ok(parts?),
            Err(e) => Err(SsaDialogLineParseError(line_num, parse_error_to_string(e)).into()),
        }
    }


    /// Parses an array of fields with the "fields info".
    ///
    /// The fields (comma seperated information) as an array like
    // `vec!["1", "0:22:43.52", "0:22:46.22", "ED-Romaji", "", "0", "0", "0", "", "{\fad(150,150)\blur0.5\bord1}some text"]`.
    fn parse_fields(line_num: usize, fields_info: &SsaFieldsInfo, v: Vec<(String, char)>) -> Result<Vec<SsaFilePart>> {
        let extract_file_parts_closure = |(i, (field, sep_char)): (_, (String, char))| -> Result<Vec<SsaFilePart>> {
            let (begin, field, end) = trim_non_destructive(&field);

            let part = if i == fields_info.start_field_idx {
                SsaFilePart::TimespanStart(Self::parse_timepoint(line_num, &field)?)
            } else if i == fields_info.end_field_idx {
                SsaFilePart::TimespanEnd(Self::parse_timepoint(line_num, &field)?)
            } else if i == fields_info.text_field_idx {
                SsaFilePart::Text(field.to_string())
            } else {
                SsaFilePart::Filler(field.to_string())
            };

            Ok(vec![SsaFilePart::Filler(begin), part, SsaFilePart::Filler(end), SsaFilePart::Filler(sep_char.to_string())])
        };

        let result = v.into_iter()
                      .enumerate()
                      .map(extract_file_parts_closure)
                      .collect::<Result<Vec<Vec<SsaFilePart>>>>()?
                      .into_iter()
                      .flat_map(|part| part)
                      .collect();
        Ok(result)
    }

    /// Something like "0:19:41.99"
    fn parse_timepoint(line: usize, s: &str) -> Result<TimePoint> {
        let parse_res = (parser(number_i64),
                         token(':'),
                         parser(number_i64),
                         token(':'),
                         parser(number_i64),
                         or(token('.'), token(':')),
                         parser(number_i64),
                         eof())
                .map(|(h, _, mm, _, ss, _, ms, _)| TimePoint::from_components(h, mm, ss, ms * 10))
                .parse(s);
        match parse_res {
            Ok(res) => Ok(res.0),
            Err(e) => Err(SsaWrongTimepointFormat(line, parse_error_to_string(e)).into()),
        }
    }
}

// ////////////////////////////////////////////////////////////////////////////////////////////////
// SSA file parts

#[derive(Debug, Clone)]
enum SsaFilePart {
    /// Spaces, field information, comments, unimportant fields, ...
    Filler(String),

    /// Timespan start of a dialogue line
    TimespanStart(TimePoint),

    /// Timespan end of a dialogue line
    TimespanEnd(TimePoint),

    /// Dialog lines
    Text(String),
}


// ////////////////////////////////////////////////////////////////////////////////////////////////
// SSA file

/// Represents a reconstructable `.ssa`/`.ass` file.
///
/// All unimportant information (for this project) are saved into `SsaFilePart::Filler(...)`, so
/// a timespan-altered file still has the same field etc.
#[derive(Debug, Clone)]
pub struct SsaFile {
    v: Vec<SsaFilePart>,
}

impl SsaFile {
    fn new(v: Vec<SsaFilePart>) -> SsaFile {
        // cleans up multiple fillers after another
        let new_file_parts = dedup_string_parts(v, |part: &mut SsaFilePart| {
            match *part {
                SsaFilePart::Filler(ref mut text) => Some(text),
                _ => None,
            }
        });

        SsaFile { v: new_file_parts }
    }

    /// This function filters out all start times and end times, and returns them ordered
    /// (="(start, end, dialog)") so they can be easily read or written to.
    ///
    /// TODO: implement a single version that takes both `&mut` and `&` (dependent on HKT).
    fn get_subtitle_entries_mut<'a>(&'a mut self) -> Vec<(&'a mut TimePoint, &'a mut TimePoint, &'a mut String)> {
        let mut startpoint_buffer: Option<&'a mut TimePoint> = None;
        let mut endpoint_buffer: Option<&'a mut TimePoint> = None;

        // the extra block satisfies the borrow checker
        let timings: Vec<_> = {
            let filter_map_closure = |part: &'a mut SsaFilePart| -> Option<(&'a mut TimePoint, &'a mut TimePoint, &'a mut String)> {
                use self::SsaFilePart::*;
                match *part {
                    TimespanStart(ref mut start) => {
                        assert_eq!(startpoint_buffer, None); // parser should have ensured that no two consecutive SSA start times exist
                        startpoint_buffer = Some(start);
                        None
                    }
                    TimespanEnd(ref mut end) => {
                        assert_eq!(endpoint_buffer, None); // parser should have ensured that no two consecutive SSA end times exist
                        endpoint_buffer = Some(end);
                        None
                    }
                    Text(ref mut text) => {
                        // reset the timepoint buffers
                        let snatched_startpoint_buffer = startpoint_buffer.take();
                        let snatched_endpoint_buffer = endpoint_buffer.take();

                        let start = snatched_startpoint_buffer.expect("SSA parser should have ensured that every line has a startpoint");
                        let end = snatched_endpoint_buffer.expect("SSA parser should have ensured that every line has a endpoint");

                        Some((start, end, text))
                    }
                    Filler(_) => None,
                }
            };

            self.v
                .iter_mut()
                .filter_map(filter_map_closure)
                .collect()
        };

        // every timespan should now consist of a beginning and a end (this should be ensured by parser)
        assert_eq!(startpoint_buffer, None);
        assert_eq!(endpoint_buffer, None);

        timings
    }
}

impl SubtitleFile for SsaFile {
    fn get_subtitle_entries(&self) -> SubtitleParserResult<Vec<SubtitleEntry>> {
        // it's unfortunate we have to clone the file before using
        // `get_subtitle_entries_mut()`, but otherwise we'd have to copy the`
        // `get_subtitle_entries_mut()` and create a non-mut-reference version
        // of it (much code duplication); I think a `clone` in this
        // not-time-critical code is acceptable, and after HKT become
        // available, this can be solved much nicer.
        let mut new_file = self.clone();
        let timings = new_file.get_subtitle_entries_mut()
                              .into_iter()
                              .map(|(&mut start, &mut end, text)| SubtitleEntry::new(TimeSpan::new(start, end), text.clone()))
                              .collect();

        Ok(timings)
    }

    fn update_subtitle_entries(&mut self, new_subtitle_entries: &[SubtitleEntry]) -> SubtitleParserResult<()> {
        let subtitle_entries = self.get_subtitle_entries_mut();
        assert_eq!(subtitle_entries.len(), new_subtitle_entries.len()); // required by specification of this function

        for ((start_ref, end_ref, text_ref), new_entry_ref) in subtitle_entries.into_iter().zip(new_subtitle_entries) {
            *start_ref = new_entry_ref.timespan.start;
            *end_ref = new_entry_ref.timespan.end;
            if let Some(ref text) = new_entry_ref.line {
                *text_ref = text.clone();
            }
        }

        Ok(())
    }

    fn to_data(&self) -> SubtitleParserResult<Vec<u8>> {
        // timing to string like "0:00:22.21"
        let fn_timing_to_string = |t: TimePoint| {
            let p = if t.msecs() < 0 { -t } else { t };
            format!("{}{}:{:02}:{:02}.{:02}",
                    if t.msecs() < 0 { "-" } else { "" },
                    p.hours(),
                    p.mins_comp(),
                    p.secs_comp(),
                    p.csecs_comp())
        };

        let fn_file_part_to_string = |part: &SsaFilePart| {
            use self::SsaFilePart::*;
            match *part {
                Filler(ref t) | Text(ref t) => t.clone(),
                TimespanStart(start) => fn_timing_to_string(start),
                TimespanEnd(end) => fn_timing_to_string(end),
            }
        };

        let result: String = self.v
                                 .iter()
                                 .map(fn_file_part_to_string)
                                 .collect();

        Ok(result.into_bytes())
    }
}