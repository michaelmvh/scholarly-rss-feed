use crate::openalex::Work;

fn normalized(text: &str) -> String {
    text.to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn has_pore(text: &str) -> bool {
    text.split_whitespace()
        .any(|word| word.starts_with("nanopore") || matches!(word, "aerolysin" | "mspa" | "clya"))
        || text.contains("nano pore")
        || text.contains("hemolysin")
        || text.contains("haemolysin")
}

fn has_analyte(text: &str) -> bool {
    let words = text.split_whitespace().collect::<Vec<_>>();
    words.iter().enumerate().any(|(index, word)| {
        word.starts_with("peptide")
            || word.starts_with("polypeptide")
            || word.starts_with("proteom")
            || word.starts_with("residue")
            || matches!(*word, "clpx" | "unfoldase")
            || (matches!(*word, "protein" | "proteins")
                && !index
                    .checked_sub(1)
                    .and_then(|previous| words.get(previous))
                    .is_some_and(|previous| {
                        matches!(*previous, "motor" | "helicase" | "polymerase")
                    })
                && !words.get(index + 1).is_some_and(|next| {
                    next.starts_with("nanopore")
                        || matches!(
                            *next,
                            "pore"
                                | "pores"
                                | "channel"
                                | "channels"
                                | "motor"
                                | "motors"
                                | "helicase"
                                | "polymerase"
                                | "engineering"
                        )
                        || (*next == "based"
                            && words.get(index + 2).is_some_and(|word| {
                                word.starts_with("nanopore") || matches!(*word, "pore" | "pores")
                            }))
                }))
    }) || text.contains("amino acid")
}

fn has_measurement(text: &str) -> bool {
    text.split_whitespace().any(|word| {
        [
            "sequenc",
            "sens",
            "read",
            "detect",
            "discriminat",
            "identif",
            "translocat",
            "fingerprint",
            "recogn",
            "unfold",
            "characteriz",
            "characteris",
            "profil",
            "modification",
            "transport",
            "classif",
            "decod",
            "analys",
        ]
        .iter()
        .any(|prefix| word.starts_with(prefix))
    })
}

fn has_evidence(text: &str) -> bool {
    has_pore(text) && has_analyte(text) && has_measurement(text)
}

fn evidence(work: &Work) -> Result<&'static str, &'static str> {
    let title = normalized(
        work.title
            .as_deref()
            .or(work.display_name.as_deref())
            .unwrap_or(""),
    );
    // A DNA-focused title is not rescued by generic protein-sensing background.
    let nucleic_acid_title = title.split_whitespace().any(|word| {
        matches!(
            word,
            "dna" | "rna" | "genome" | "genomic" | "genomics" | "transcriptome"
        )
    });
    if nucleic_acid_title && !has_evidence(&title) {
        return Err("nucleic-acid-focused title without protein measurement evidence");
    }
    if has_evidence(&title) {
        return Ok("title: pore + protein/peptide analyte + measurement");
    }
    let Some(abstract_text) = work.abstract_text() else {
        return Err("missing abstract and insufficient title evidence");
    };
    // Require co-occurrence within a sentence, not unrelated abstract background.
    if abstract_text
        .split(['.', '!', '?', ';'])
        .any(|sentence| has_evidence(&normalized(sentence)))
    {
        return Ok("abstract sentence: pore + protein/peptide analyte + measurement");
    }
    Err("insufficient protein nanopore measurement evidence")
}

pub(crate) fn retain_relevant(works: &mut Vec<Work>) {
    let candidates = works.len();
    let mut missing_abstract = 0;
    works.retain(|work| match evidence(work) {
        Ok(reason) => {
            eprintln!(
                "Protein nanopore inclusion {}: {reason}",
                work.id.as_deref().unwrap_or("(no provider ID)")
            );
            true
        }
        Err(reason) => {
            if work.abstract_text().is_none() {
                missing_abstract += 1;
                eprintln!(
                    "Protein nanopore exclusion {}: {reason}",
                    work.id.as_deref().unwrap_or("(no provider ID)")
                );
            }
            false
        }
    });
    eprintln!(
        "Protein nanopore relevance: accepted {} of {candidates} candidates; {missing_abstract} excluded candidates lack abstracts",
        works.len(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paper(title: &str, abstract_text: Option<&str>) -> Work {
        serde_json::from_value(serde_json::json!({
            "title": title,
            "abstract_text_override": abstract_text,
        }))
        .unwrap()
    }

    #[test]
    fn accepts_landmarks_and_protein_measurements_without_abstracts() {
        for title in [
            "Unfoldase-mediated protein translocation through an alpha-hemolysin nanopore",
            "Multi-pass, single-molecule nanopore reading of long protein strands",
            "Digestion and Orthogonal Click Conjugation Enables Nanopore Reading of Proteins",
            "Sample preparation for direct nanopore based protein identification via tryptic peptide recognition",
            "Single amino-acid discrimination using aerolysin",
            "Solid-state nanopore sensing of peptides",
            "Protein sequencing using DNA adapters and a nanopore",
            "Nanopore detection of post-translational modifications in proteins",
            "Motor-controlled protein transport through a nanopore",
            "Classification of nanopore signals from peptides",
        ] {
            assert!(evidence(&paper(title, None)).is_ok(), "{title}");
        }
    }

    #[test]
    fn accepts_abstract_evidence_when_the_title_omits_nanopores() {
        let work = paper(
            "Single-molecule analysis of long chains",
            Some("We demonstrate nanopore sequencing of proteins using a DNA adapter."),
        );
        assert_eq!(
            evidence(&work).unwrap(),
            "abstract sentence: pore + protein/peptide analyte + measurement"
        );
        assert!(evidence(&paper(
            "Engineering an aerolysin sensor",
            Some("The nanopore discriminates individual amino acids."),
        ))
        .is_ok());
    }

    #[test]
    fn rejects_material_only_and_nucleic_acid_background_matches() {
        for title in [
            "Protein nanopore sequencing of DNA",
            "Protein nanopores for RNA sensing",
            "Protein-based nanopores for DNA detection",
            "DNA sequencing using a motor protein and a nanopore",
            "RNA nanopore sequencing with a helicase protein",
            "DNA storage using nanopore sequencing",
            "Nanoporous protein materials for catalysis",
            "Engineering protein nanopores",
            "Unfoldase-mediated protein degradation",
            "Genome assembly using nanopore reads",
        ] {
            assert!(evidence(&paper(title, None)).is_err(), "{title}");
        }
        assert!(evidence(&paper(
            "DNA sequencing with an engineered protein nanopore",
            Some("Nanopores also enable protein sensing. Here we sequence DNA."),
        ))
        .is_err());
        assert!(evidence(&paper(
            "A new measurement platform",
            Some("Nanopores sequence DNA. Protein identification uses mass spectrometry."),
        ))
        .is_err());
    }

    #[test]
    fn missing_metadata_needs_title_evidence_not_an_author_match() {
        let mut unknown = paper("New molecular tools", None);
        unknown.matched_author_names.push("Jeff Nivala".to_string());
        let mut works = vec![unknown, paper("Nanopore reading of proteins", None)];
        retain_relevant(&mut works);
        assert_eq!(works.len(), 1);
        assert_eq!(
            works[0].title.as_deref(),
            Some("Nanopore reading of proteins")
        );
    }
}
