public class Example {
    private String name;
    
    public Example(String name) {
        this.name = name;
    }
    
    public void greet() {
        System.out.println("Hello, " + name);
    }
    
    public static void main(String[] args) {
        Example example = new Example("Java");
        example.greet();
    }
}
