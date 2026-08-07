import * as path from "node:path";
import * as cdk from "aws-cdk-lib";
import { Construct } from "constructs";
import * as s3 from "aws-cdk-lib/aws-s3";
import * as s3n from "aws-cdk-lib/aws-s3-notifications";
import * as dynamodb from "aws-cdk-lib/aws-dynamodb";
import * as lambda from "aws-cdk-lib/aws-lambda";
import * as destinations from "aws-cdk-lib/aws-lambda-destinations";
import * as sqs from "aws-cdk-lib/aws-sqs";
import * as iam from "aws-cdk-lib/aws-iam";
import * as scheduler from "aws-cdk-lib/aws-scheduler";
import * as cognito from "aws-cdk-lib/aws-cognito";
import * as cloudfront from "aws-cdk-lib/aws-cloudfront";
import * as origins from "aws-cdk-lib/aws-cloudfront-origins";
import * as s3deploy from "aws-cdk-lib/aws-s3-deployment";
import * as apigwv2 from "aws-cdk-lib/aws-apigatewayv2";
import { HttpLambdaIntegration } from "aws-cdk-lib/aws-apigatewayv2-integrations";
import { HttpUserPoolAuthorizer } from "aws-cdk-lib/aws-apigatewayv2-authorizers";
import { PythonFunction } from "@aws-cdk/aws-lambda-python-alpha";

// Cross-region inference profile for the Japan regions.
// To deploy elsewhere, change the prefix as well (us., eu., and so on)
const MODEL_ID = "jp.anthropic.claude-haiku-4-5-20251001-v1:0";
const PROFILE_KEY = "config/profile.yaml";
// Fixed port the client's browser login listens on
const CLIENT_CALLBACK_URL = "http://localhost:8765/callback";

const lambdasDir = path.join(__dirname, "..", "..", "lambdas");

export class ScreenMemoryStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props?: cdk.StackProps) {
    super(scope, id, props);

    // ---- Data bucket ----
    const bucket = new s3.Bucket(this, "DataBucket", {
      bucketName: `screen-memory-data-${this.account}`,
      blockPublicAccess: s3.BlockPublicAccess.BLOCK_ALL,
      encryption: s3.BucketEncryption.S3_MANAGED,
      removalPolicy: cdk.RemovalPolicy.RETAIN,
      lifecycleRules: [
        {
          id: "expire-raw",
          prefix: "raw/",
          expiration: cdk.Duration.days(14),
        },
      ],
    });

    // ---- DynamoDB table ----
    const table = new dynamodb.Table(this, "ActivityTable", {
      tableName: "screen-memory-activity",
      partitionKey: { name: "pk", type: dynamodb.AttributeType.STRING },
      sortKey: { name: "sk", type: dynamodb.AttributeType.STRING },
      billingMode: dynamodb.BillingMode.PAY_PER_REQUEST,
      removalPolicy: cdk.RemovalPolicy.RETAIN,
    });

    // Invoking through a cross-region inference profile requires both the profile ARN
    // and the ARN of the foundation model actually called (with a wildcard region)
    const bedrockInvokeStatement = new iam.PolicyStatement({
      actions: ["bedrock:InvokeModel"],
      resources: [
        `arn:aws:bedrock:${this.region}:${this.account}:inference-profile/${MODEL_ID}`,
        "arn:aws:bedrock:*::foundation-model/anthropic.claude-haiku-4-5-20251001-v1:0",
      ],
    });

    // ---- Analyze Lambda ----
    const analyzeDlq = new sqs.Queue(this, "AnalyzeDlq", {
      queueName: "screen-memory-analyze-dlq",
      retentionPeriod: cdk.Duration.days(14),
    });

    const analyzeFn = new PythonFunction(this, "AnalyzeFunction", {
      functionName: "screen-memory-analyze",
      entry: path.join(lambdasDir, "analyze"),
      index: "handler.py",
      handler: "lambda_handler",
      bundling: { assetExcludes: ["tests", "__pycache__", ".pytest_cache"] },
      runtime: lambda.Runtime.PYTHON_3_13,
      architecture: lambda.Architecture.ARM_64,
      memorySize: 512,
      timeout: cdk.Duration.seconds(60),
      reservedConcurrentExecutions: 2,
      environment: {
        BUCKET_NAME: bucket.bucketName,
        TABLE_NAME: table.tableName,
        MODEL_ID,
        PROFILE_KEY,
      },
    });
    analyzeFn.configureAsyncInvoke({
      retryAttempts: 2,
      onFailure: new destinations.SqsDestination(analyzeDlq),
    });
    bucket.addEventNotification(
      s3.EventType.OBJECT_CREATED,
      new s3n.LambdaDestination(analyzeFn),
      { prefix: "raw/", suffix: ".json" },
    );
    analyzeFn.addToRolePolicy(
      new iam.PolicyStatement({
        actions: ["s3:GetObject"],
        resources: [`${bucket.bucketArn}/raw/*`, `${bucket.bucketArn}/config/*`],
      }),
    );
    // Without ListBucket, GetObject / HeadObject on a missing object returns
    // AccessDenied instead of 404, which breaks the fallback for an absent profile
    analyzeFn.addToRolePolicy(
      new iam.PolicyStatement({
        actions: ["s3:ListBucket"],
        resources: [bucket.bucketArn],
      }),
    );
    analyzeFn.addToRolePolicy(
      new iam.PolicyStatement({
        actions: ["dynamodb:PutItem"],
        resources: [table.tableArn],
      }),
    );
    analyzeFn.addToRolePolicy(bedrockInvokeStatement);

    // ---- Daily report Lambda ----
    const reportFn = new PythonFunction(this, "ReportFunction", {
      functionName: "screen-memory-report",
      entry: path.join(lambdasDir, "report"),
      index: "index.py",
      handler: "handler",
      bundling: { assetExcludes: ["tests", "__pycache__", ".pytest_cache"] },
      runtime: lambda.Runtime.PYTHON_3_13,
      architecture: lambda.Architecture.ARM_64,
      memorySize: 512,
      timeout: cdk.Duration.seconds(300),
      environment: {
        BUCKET_NAME: bucket.bucketName,
        TABLE_NAME: table.tableName,
        MODEL_ID,
        PROFILE_KEY,
      },
    });
    reportFn.addToRolePolicy(
      new iam.PolicyStatement({
        actions: ["dynamodb:Query", "dynamodb:PutItem"],
        resources: [table.tableArn],
      }),
    );
    reportFn.addToRolePolicy(
      new iam.PolicyStatement({
        actions: ["s3:GetObject"],
        resources: [`${bucket.bucketArn}/config/*`],
      }),
    );
    reportFn.addToRolePolicy(
      new iam.PolicyStatement({
        actions: ["s3:ListBucket"],
        resources: [bucket.bucketArn],
      }),
    );
    reportFn.addToRolePolicy(
      new iam.PolicyStatement({
        actions: ["s3:PutObject"],
        resources: [`${bucket.bucketArn}/reports/*`],
      }),
    );
    reportFn.addToRolePolicy(bedrockInvokeStatement);

    const schedulerRole = new iam.Role(this, "DailyReportSchedulerRole", {
      assumedBy: new iam.ServicePrincipal("scheduler.amazonaws.com"),
    });
    reportFn.grantInvoke(schedulerRole);
    new scheduler.CfnSchedule(this, "DailyReportSchedule", {
      name: "screen-memory-daily-report",
      scheduleExpression: "cron(55 23 * * ? *)",
      scheduleExpressionTimezone: "Asia/Tokyo",
      flexibleTimeWindow: { mode: "OFF" },
      target: {
        arn: reportFn.functionArn,
        roleArn: schedulerRole.roleArn,
      },
    });

    // ---- Read API ----
    const apiFn = new lambda.Function(this, "ApiFunction", {
      functionName: "screen-memory-api",
      code: lambda.Code.fromAsset(path.join(lambdasDir, "api")),
      handler: "index.handler",
      runtime: lambda.Runtime.PYTHON_3_13,
      architecture: lambda.Architecture.ARM_64,
      memorySize: 256,
      timeout: cdk.Duration.seconds(15),
      environment: {
        BUCKET_NAME: bucket.bucketName,
        TABLE_NAME: table.tableName,
      },
    });
    apiFn.addToRolePolicy(
      new iam.PolicyStatement({
        actions: ["dynamodb:Query"],
        resources: [table.tableArn],
      }),
    );
    apiFn.addToRolePolicy(
      new iam.PolicyStatement({
        actions: ["s3:GetObject"],
        resources: [`${bucket.bucketArn}/reports/*`],
      }),
    );
    // Without ListBucket, GetObject on a missing report returns AccessDenied
    // instead of 404, so the function cannot respond with a 404
    apiFn.addToRolePolicy(
      new iam.PolicyStatement({
        actions: ["s3:ListBucket"],
        resources: [bucket.bucketArn],
      }),
    );
    // ---- Web UI delivery (S3 + CloudFront) ----
    const webBucket = new s3.Bucket(this, "WebBucket", {
      bucketName: `screen-memory-web-${this.account}`,
      blockPublicAccess: s3.BlockPublicAccess.BLOCK_ALL,
      encryption: s3.BucketEncryption.S3_MANAGED,
      // The contents can be regenerated from web/ by redeploying
      removalPolicy: cdk.RemovalPolicy.DESTROY,
      autoDeleteObjects: true,
    });
    const distribution = new cloudfront.Distribution(this, "WebDistribution", {
      defaultBehavior: {
        origin: origins.S3BucketOrigin.withOriginAccessControl(webBucket),
        viewerProtocolPolicy: cloudfront.ViewerProtocolPolicy.REDIRECT_TO_HTTPS,
      },
      defaultRootObject: "index.html",
      // The cheapest price class that still includes the Japanese edge locations
      priceClass: cloudfront.PriceClass.PRICE_CLASS_200,
    });

    // ---- Authentication (Cognito) ----
    // Uploads (direct S3 PUT), the read API, and the Web UI all authenticate against
    // the same user pool. This removes the need to issue access keys, and leaves room
    // to federate an OIDC / SAML identity provider later.
    const userPool = new cognito.UserPool(this, "UserPool", {
      userPoolName: "screen-memory-users",
      selfSignUpEnabled: false,
      signInAliases: { username: true },
      removalPolicy: cdk.RemovalPolicy.RETAIN,
    });
    const userPoolDomain = userPool.addDomain("Domain", {
      cognitoDomain: { domainPrefix: `screen-memory-${this.account}` },
    });
    const userPoolClient = new cognito.UserPoolClient(this, "UserPoolClient", {
      userPool,
      generateSecret: false,
      // Avoid SRP so you can sign in with just a password from the AWS CLI to check things work
      authFlows: { userPassword: true },
      // The Web UI and the client sign in through the Hosted UI with authorization code + PKCE
      oAuth: {
        flows: { authorizationCodeGrant: true },
        scopes: [cognito.OAuthScope.OPENID],
        callbackUrls: [
          `https://${distribution.distributionDomainName}/`,
          CLIENT_CALLBACK_URL,
        ],
        logoutUrls: [`https://${distribution.distributionDomainName}/`],
      },
      refreshTokenValidity: cdk.Duration.days(3650),
    });

    const httpApi = new apigwv2.HttpApi(this, "HttpApi", {
      apiName: "screen-memory-api",
      corsPreflight: {
        allowOrigins: [`https://${distribution.distributionDomainName}`],
        allowHeaders: ["authorization"],
        allowMethods: [apigwv2.CorsHttpMethod.GET],
        maxAge: cdk.Duration.hours(1),
      },
      // Verify the Cognito ID token at the gateway; no auth code lives in the Lambda
      defaultAuthorizer: new HttpUserPoolAuthorizer("ApiAuthorizer", userPool, {
        userPoolClients: [userPoolClient],
      }),
    });
    const apiIntegration = new HttpLambdaIntegration("ApiIntegration", apiFn);
    httpApi.addRoutes({
      path: "/days/{date}/records",
      methods: [apigwv2.HttpMethod.GET],
      integration: apiIntegration,
    });
    httpApi.addRoutes({
      path: "/days/{date}/report",
      methods: [apigwv2.HttpMethod.GET],
      integration: apiIntegration,
    });

    new s3deploy.BucketDeployment(this, "WebDeployment", {
      destinationBucket: webBucket,
      sources: [
        s3deploy.Source.asset(path.join(__dirname, "..", "..", "web")),
        s3deploy.Source.jsonData("config.json", {
          apiEndpoint: httpApi.apiEndpoint,
          cognitoDomain: userPoolDomain.baseUrl(),
          clientId: userPoolClient.userPoolClientId,
        }),
      ],
      distribution,
      distributionPaths: ["/*"],
    });

    const identityPool = new cognito.CfnIdentityPool(this, "IdentityPool", {
      identityPoolName: "screen-memory-identity",
      allowUnauthenticatedIdentities: false,
      cognitoIdentityProviders: [
        {
          providerName: userPool.userPoolProviderName,
          clientId: userPoolClient.userPoolClientId,
        },
      ],
    });

    const authenticatedRole = new iam.Role(this, "UploaderAuthenticatedRole", {
      assumedBy: new iam.FederatedPrincipal(
        "cognito-identity.amazonaws.com",
        {
          StringEquals: {
            "cognito-identity.amazonaws.com:aud": identityPool.ref,
          },
          "ForAnyValue:StringLike": {
            "cognito-identity.amazonaws.com:amr": "authenticated",
          },
        },
        "sts:AssumeRoleWithWebIdentity",
      ),
    });
    authenticatedRole.addToPolicy(
      new iam.PolicyStatement({
        actions: ["s3:PutObject"],
        resources: [`${bucket.bucketArn}/raw/*`],
      }),
    );
    new cognito.CfnIdentityPoolRoleAttachment(this, "IdentityPoolRoles", {
      identityPoolId: identityPool.ref,
      roles: { authenticated: authenticatedRole.roleArn },
    });

    // ---- Outputs ----
    new cdk.CfnOutput(this, "BucketName", { value: bucket.bucketName });
    new cdk.CfnOutput(this, "TableName", { value: table.tableName });
    new cdk.CfnOutput(this, "ApiEndpoint", { value: httpApi.apiEndpoint });
    new cdk.CfnOutput(this, "UserPoolId", { value: userPool.userPoolId });
    new cdk.CfnOutput(this, "UserPoolClientId", {
      value: userPoolClient.userPoolClientId,
    });
    new cdk.CfnOutput(this, "IdentityPoolId", { value: identityPool.ref });
    new cdk.CfnOutput(this, "CognitoDomain", {
      value: userPoolDomain.baseUrl(),
    });
    new cdk.CfnOutput(this, "WebUrl", {
      value: `https://${distribution.distributionDomainName}`,
    });
  }
}
