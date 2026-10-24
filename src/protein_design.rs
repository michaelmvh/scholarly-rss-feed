use crate::openalex::Work;

fn normalized(text: &str) -> String {
    text.to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn protein_subject(word: &str) -> bool {
    matches!(
        word,
        "protein"
            | "proteins"
            | "miniprotein"
            | "miniproteins"
            | "peptide"
            | "peptides"
            | "polypeptide"
            | "polypeptides"
            | "antibody"
            | "antibodies"
            | "nanobody"
            | "nanobodies"
            | "enzyme"
            | "enzymes"
            | "minibinder"
            | "minibinders"
            | "macrocycle"
            | "macrocycles"
            | "protease"
            | "proteases"
            | "metalloprotease"
            | "metalloproteases"
            | "polymerase"
            | "polymerases"
    ) || word.ends_with("enzyme")
        || word.ends_with("enzymes")
}

fn design_action(word: &str) -> bool {
    matches!(
        word,
        "design"
            | "designs"
            | "designed"
            | "designing"
            | "redesign"
            | "redesigned"
            | "redesigning"
            | "engineering"
            | "engineered"
            | "engineer"
            | "optimization"
            | "optimisation"
            | "optimizing"
            | "optimising"
    )
}

fn has_evidence(text: &str) -> bool {
    let words = text.split_whitespace().collect::<Vec<_>>();
    let protein = words.iter().any(|word| protein_subject(word));
    let design = words.iter().any(|word| design_action(word));
    let model = words.iter().any(|word| {
        matches!(
            *word,
            "boltz"
                | "boltz1"
                | "boltz2"
                | "boltzgen"
                | "rfdiffusion"
                | "rfdiffusion2"
                | "rfdiffusion3"
                | "proteinmpnn"
                | "ligandmpnn"
                | "bindcraft"
                | "alphafold"
                | "alphafold2"
                | "alphafold3"
                | "esmfold"
        )
    });
    let nearby_design = words.iter().enumerate().any(|(index, word)| {
        protein_subject(word)
            && words[index.saturating_sub(5)..(index + 6).min(words.len())]
                .iter()
                .any(|word| design_action(word))
    });
    let inverse_folding = text.contains("inverse folding")
        && (protein || !words.iter().any(|word| matches!(*word, "rna" | "dna")));
    if nearby_design || inverse_folding || model && design {
        return true;
    }

    let prediction = words.iter().any(|word| {
        matches!(
            *word,
            "prediction" | "predicting" | "predict" | "predicts" | "modeling" | "modelling"
        )
    });
    let structural_task = words.iter().any(|word| {
        matches!(
            *word,
            "structure" | "structures" | "structural" | "affinity"
        )
    }) || [
        "interaction prediction",
        "interaction modeling",
        "interaction modelling",
        "interactions prediction",
        "interactions modeling",
        "interactions modelling",
    ]
    .iter()
    .any(|task| text.contains(task));
    let biomolecular = words.contains(&"biomolecular");
    let method = model
        || words.iter().any(|word| {
            matches!(
                *word,
                "model" | "models" | "learning" | "neural" | "method" | "methods"
            )
        });
    let generative = words.contains(&"generative")
        && words
            .iter()
            .any(|word| matches!(*word, "sequence" | "sequences" | "structure" | "structures"));

    (protein || biomolecular || model)
        && (prediction && structural_task && method || protein && generative)
}

fn evidence(work: &Work) -> Result<&'static str, &'static str> {
    let title = normalized(
        work.title
            .as_deref()
            .or(work.display_name.as_deref())
            .unwrap_or(""),
    );
    let title_words = title.split_whitespace().collect::<Vec<_>>();
    if title_words.contains(&"franchise")
        && title_words
            .iter()
            .any(|word| matches!(*word, "license" | "restaurant"))
    {
        return Err("commercial franchise record");
    }
    if has_evidence(&title) {
        return Ok("title: protein design or enabling biomolecular model");
    }
    if title
        .split_whitespace()
        .any(|word| matches!(word, "rna" | "dna" | "genome" | "genomic"))
        && !title.split_whitespace().any(protein_subject)
    {
        return Err("nucleic-acid-focused title without protein design evidence");
    }
    let Some(abstract_text) = work.abstract_text() else {
        return Err("missing abstract and insufficient title evidence");
    };
    if abstract_text.split(['.', '!', '?', ';']).any(|sentence| {
        let sentence = normalized(sentence);
        // Background descriptions of existing therapeutics are not new design work.
        let contribution = sentence
            .split_whitespace()
            .any(|word| matches!(word, "we" | "our"))
            || ["this work", "this study", "this paper", "here we"]
                .iter()
                .any(|phrase| sentence.contains(phrase));
        contribution && has_evidence(&sentence)
    }) {
        return Ok("abstract sentence: protein design or enabling biomolecular model");
    }
    Err("insufficient protein design or enabling-model evidence")
}

pub(crate) fn retain_relevant(works: &mut Vec<Work>) {
    let candidates = works.len();
    let mut missing_abstract = 0;
    works.retain(|work| {
        let (accepted, reason) = match evidence(work) {
            Ok(reason) => (true, reason),
            Err(reason) => {
                if work.abstract_text().is_none() {
                    missing_abstract += 1;
                }
                (false, reason)
            }
        };
        eprintln!(
            "Protein design {} {}: {reason}",
            if accepted { "inclusion" } else { "exclusion" },
            work.id.as_deref().unwrap_or("(no provider ID)"),
        );
        accepted
    });
    eprintln!(
        "Protein design relevance: accepted {} of {candidates} candidates; {missing_abstract} excluded candidates lack abstracts",
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
    fn accepts_design_and_enabling_model_landmarks_without_abstracts() {
        for title in [
            "De novo design of hyperstable constrained peptides",
            "Robust deep learning-based protein sequence design using ProteinMPNN",
            "De novo design of protein structure and function with RFdiffusion",
            "BoltzGen: Toward Universal Binder Design",
            "Boltz-1: Democratizing Biomolecular Interaction Modeling",
            "Boltz-2: Towards Accurate and Efficient Binding Affinity Prediction",
            "Accurate structure prediction of biomolecular interactions with AlphaFold 3",
            "Protein inverse folding with diffusion",
            "Generative models of protein sequences",
            "Antibody engineering for improved specificity",
            "Computational enzyme design",
            "Computational design of metalloproteases",
            "De novo design of cysteine proteases",
            "De novo design of artificial metalloenzyme photocatalysts",
            "Design of self-assembling protein nanomaterials",
            "Vilya-1: An all-atom foundation model for macrocycle structure prediction and design",
            "Evaluation of generative protein structure models",
        ] {
            assert!(evidence(&paper(title, None)).is_ok(), "{title}");
        }
    }

    #[test]
    fn accepts_sentence_evidence_but_not_unrelated_abstract_fragments() {
        assert!(evidence(&paper(
            "Programmable molecular recognition",
            Some("We design miniproteins that bind selected cell-surface targets."),
        ))
        .is_ok());
        assert!(evidence(&paper(
            "A new molecular modeling framework",
            Some("Our neural model predicts protein structures and binding affinity."),
        ))
        .is_ok());
        assert!(evidence(&paper(
            "A new analysis framework",
            Some("Proteins were measured by mass spectrometry. We design a clinical trial."),
        ))
        .is_err());
    }

    #[test]
    fn excludes_unrelated_author_papers_and_incidental_model_mentions() {
        for title in [
            "Deep learning for mammography screening",
            "Graph neural networks for combinatorial optimization",
            "Protein expression in breast cancer",
            "Crystal structure of a viral protein",
            "Small molecule drug design using molecular docking",
            "Protein language models for subcellular localization",
            "Boltzmann distributions in statistical mechanics",
            "Biomolecular interaction dataset",
            "Protein sequencing using a nanopore",
            "Recent progress in functional polymer binders for zinc metal batteries",
            "Recycled binder availability in asphalt mix design",
            "RNA inverse folding with geometric diffusion",
            "RNASeek: A generative foundation model for RNA design",
            "Exclusive franchise license for a restaurant using a proprietary protein design system",
        ] {
            assert!(evidence(&paper(title, None)).is_err(), "{title}");
        }
        let mut unknown = paper("New molecular tools", None);
        unknown.matched_author_names.push("Regina Barzilay".into());
        let incidental = paper(
            "Protein expression in cancer cells",
            Some("Boltz-2 was used for preprocessing. We measure protein expression."),
        );
        let background = paper(
            "Amyloid clearance in a clinical trial",
            Some("Donanemab is an antibody designed to target amyloid plaques."),
        );
        let rna_background = paper(
            "RNA co-design with flow matching",
            Some("We build on advances in protein design to model RNA sequences."),
        );
        let unrelated_design = paper(
            "Measuring cell populations",
            Some("We measure protein abundance while applying a new experimental design."),
        );
        let mut works = vec![
            unknown,
            incidental,
            background,
            rna_background,
            unrelated_design,
            paper("Enzyme design", None),
        ];
        retain_relevant(&mut works);
        assert_eq!(works.len(), 1);
        assert_eq!(works[0].best_title(), "Enzyme design");
    }
}
