#[cfg(test)]
mod tests {
    use crate::{AXElement, MacAxController};

    #[test]
    fn test_ax_element_to_string() {
        let element = AXElement {
            role: "button".to_string(),
            title: Some("Click Me".to_string()),
            value: None,
            label: Some("Submit Button".to_string()),
            identifier: Some("submitBtn".to_string()),
            enabled: true,
            focused: false,
            position: Some((100.0, 200.0)),
            size: Some((80.0, 30.0)),
            children_count: 0,
        };

        let string_repr = element.to_string();
        assert!(string_repr.contains("Role: button"));
        assert!(string_repr.contains("Title: Click Me"));
        assert!(string_repr.contains("Label: Submit Button"));
        assert!(string_repr.contains("ID: submitBtn"));
        assert!(string_repr.contains("Enabled: true"));
        assert!(string_repr.contains("Position: (100, 200)"));
        assert!(string_repr.contains("Size: (80, 30)"));
    }

    #[test]
    fn test_controller_creation() {
        // Just test that we can create a controller
        // Actual functionality requires macOS and permissions
        let result = MacAxController::new();
        assert!(result.is_ok());
    }
}
